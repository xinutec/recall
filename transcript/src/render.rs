//! The lines a clip shows, as a function of what was heard and what people did
//! (#1915). The only place a [`Line`] is made.
//!
//! Every rule is here once:
//!
//! 1. A model segment that is a repetition loop, has no words, or is a phrase
//!    the model writes over silence where no speech was heard is dropped, and
//!    the drop says why.
//! 2. Each remaining word is spoken by the diarized speaker at its midpoint;
//!    runs shorter than [`MIN_TURN_US`] are folded into a neighbour.
//! 3. A person's words, "nobody spoke" or "can't make it out" own their span:
//!    the model's words under it give way. A later act replaces an earlier one
//!    whose middle it covers.
//! 4. Inside a model segment, a sentence goes to whoever speaks most of it; a
//!    line breaks where the segment ends or the speaker changes between
//!    sentences, never inside one.
//! 5. A line's speaker is, in order: a person's naming of that stretch, a
//!    person's naming of that voice, the voiceprint guess, the diarized label.
//!    A person's words take the voice diarized at their midpoint, like a word.

use crate::quality::{NEAR_SILENT_S, is_implausibly_slow, is_silence_phrase, speech_inside};
use crate::text::{is_repetition_loop, is_wordless};
use crate::voice::{Voiceprint, match_one};
use crate::{Act, Clip, Edit, EditId, Instant, Name, Span};

/// Diarized runs shorter than this, in microseconds, are alignment jitter or a
/// backchannel, and go to a neighbour.
pub const MIN_TURN_US: i64 = 500_000;

/// What the model heard in the clip; times in seconds from the clip's start.
#[derive(Debug, Clone, PartialEq)]
pub struct Heard {
    pub language: Option<String>,
    pub segments: Vec<HeardSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeardSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// `None` for results stored without word timings: the segment is then one
    /// word.
    pub words: Option<Vec<HeardWord>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeardWord {
    pub start: f64,
    pub end: f64,
    /// Whisper words carry their own leading space; joined verbatim.
    pub text: String,
    pub probability: Option<f64>,
}

/// Who spoke when, and each speaker's print; seconds from the clip's start.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Voices {
    pub turns: Vec<VoiceTurn>,
    pub prints: Vec<(String, Vec<f64>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceTurn {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
}

/// What the speech detector found in the clip.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Speech {
    /// Seconds of speech in the whole clip, when measured.
    pub seconds: Option<f64>,
    /// Where it is, seconds from the clip's start, when placed.
    pub regions: Option<Vec<(f64, f64)>>,
}

/// Everything a clip's lines depend on.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub clip: &'a Clip,
    pub heard: Option<&'a Heard>,
    pub voices: Option<&'a Voices>,
    pub speech: &'a Speech,
    /// The whole log; render keeps what touches this clip or its source.
    pub edits: &'a [Edit],
    pub enrolled: &'a [Voiceprint],
}

/// One line as shown. Made only by [`render`].
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Line {
    span: Span,
    text: String,
    by: Author,
    speaker: Option<Speaker>,
    language: Option<String>,
    confidence: Option<f64>,
    checked: bool,
}

impl Line {
    pub fn span(&self) -> Span {
        self.span
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn by(&self) -> Author {
        self.by
    }
    pub fn speaker(&self) -> Option<&Speaker> {
        self.speaker.as_ref()
    }
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
    /// The model's mean word probability; zero when a rule doubts the line;
    /// `None` for a person's words.
    pub fn confidence(&self) -> Option<f64> {
        self.confidence
    }
    /// A person listened and vouches for the words.
    pub fn checked(&self) -> bool {
        self.checked
    }
}

/// Whose words a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Author {
    Model,
    Person(EditId),
}

/// Who a line is attributed to, and on what grounds.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum Speaker {
    /// A person named whoever spoke over this stretch.
    Named { name: Name, by: EditId },
    /// A person named this voice for the whole session.
    Voice { name: Name, by: EditId },
    /// The voiceprint's best match, and how sure.
    Guess { name: String, score: f64 },
    /// Only the diarizer's label: one voice, not yet known.
    Cluster(String),
}

/// Model text that is not shown, and why.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Dropped {
    pub span: Span,
    pub text: String,
    pub why: Why,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Why {
    RepetitionLoop,
    Wordless,
    /// A phrase the model writes over silence, where none was heard.
    SilencePhrase,
    /// A person's words replaced these.
    Replaced(EditId),
    /// A person said nobody spoke here.
    NoSpeech(EditId),
    /// A person said the words cannot be made out.
    Unintelligible(EditId),
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
pub struct Rendered {
    pub lines: Vec<Line>,
    pub dropped: Vec<Dropped>,
}

/// The clip's lines. Pure: the same input always gives the same lines.
pub fn render(input: &Input<'_>) -> Rendered {
    let acts = active(input.edits, input.clip);
    let mut out = Rendered::default();
    let words = model_words(input, &mut out);
    let owned = person_layer(&acts);
    let mut kept = Vec::new();
    for word in words {
        match owned.iter().rev().find(|o| o.over.contains(word.mid)) {
            Some(Owned { content, id, .. }) => out.dropped.push(Dropped {
                span: word.span,
                text: word.text.clone(),
                why: match content {
                    Content::Words { .. } => Why::Replaced(*id),
                    Content::NoSpeech => Why::NoSpeech(*id),
                    Content::Unintelligible => Why::Unintelligible(*id),
                },
            }),
            None => kept.push(word),
        }
    }
    let speakers = Speakers::new(input, &acts);
    out.lines.extend(model_lines(&kept, &speakers, input.heard));
    for Owned {
        span, content, id, ..
    } in &owned
    {
        if let Content::Words { text, checked } = content {
            let mid = span_mid(*span);
            let seconds = (mid.micros() - input.clip.start.micros()) as f64 / 1e6;
            let cluster = speaker_at(seconds, speakers.turns);
            out.lines.push(Line {
                span: *span,
                text: text.clone(),
                by: Author::Person(*id),
                speaker: speakers.resolve(mid, cluster.as_deref()),
                language: input.heard.and_then(|h| h.language.clone()),
                confidence: None,
                checked: *checked,
            });
        }
    }
    out.lines.sort_by_key(|l| (l.span.start(), l.span.end()));
    out
}

/// The acts in force for this clip: retracted ones removed (and a retracted
/// retraction restores its target), in the order they were done.
fn active<'a>(edits: &'a [Edit], clip: &Clip) -> Vec<&'a Edit> {
    let mut ordered: Vec<&Edit> = edits.iter().collect();
    ordered.sort_by_key(|e| e.id);
    let mut retracted = std::collections::BTreeSet::new();
    for edit in ordered.iter().rev() {
        if retracted.contains(&edit.id) {
            continue;
        }
        if let Act::Retract { edit: target } = edit.act {
            retracted.insert(target);
        }
    }
    ordered
        .into_iter()
        .filter(|e| !retracted.contains(&e.id))
        .filter(|e| match &e.act {
            Act::Words { clip: c, .. }
            | Act::Speaker { clip: c, .. }
            | Act::NoSpeech { clip: c, .. }
            | Act::Unintelligible { clip: c, .. } => *c == clip.id,
            Act::Voice { source, .. } | Act::Language { source, .. } => *source == clip.source,
            Act::Retract { .. } => false,
        })
        .collect()
}

#[derive(Debug, Clone)]
struct Word {
    span: Span,
    mid: Instant,
    text: String,
    probability: Option<f64>,
    segment: usize,
    /// Clip-relative midpoint, for the diarizer's clock.
    at: f64,
}

/// The model's words that survive the text rules, with absolute times.
fn model_words(input: &Input<'_>, out: &mut Rendered) -> Vec<Word> {
    let Some(heard) = input.heard else {
        return Vec::new();
    };
    let start = input.clip.start;
    let place = |a: f64, b: f64| Span::new(start.plus_seconds(a)?, start.plus_seconds(b.max(a))?);
    let mut words = Vec::new();
    for (index, segment) in heard.segments.iter().enumerate() {
        let Some(span) = place(segment.start, segment.end) else {
            continue;
        };
        let why = if is_repetition_loop(&segment.text) {
            Some(Why::RepetitionLoop)
        } else if is_wordless(&segment.text) {
            Some(Why::Wordless)
        } else if invented(&segment.text, segment.start, segment.end, input.speech) {
            Some(Why::SilencePhrase)
        } else {
            None
        };
        if let Some(why) = why {
            out.dropped.push(Dropped {
                span,
                text: segment.text.trim().to_owned(),
                why,
            });
            continue;
        }
        let timed: Vec<(f64, f64, &str, Option<f64>)> = match segment.words.as_deref() {
            Some(list) if !list.is_empty() => list
                .iter()
                .map(|w| (w.start, w.end, w.text.as_str(), w.probability))
                .collect(),
            _ => vec![(segment.start, segment.end, segment.text.as_str(), None)],
        };
        for (a, b, text, probability) in timed {
            if let Some(span) = place(a, b) {
                words.push(Word {
                    span,
                    mid: span_mid(span),
                    text: text.to_owned(),
                    probability,
                    segment: index,
                    at: f64::midpoint(a, b.max(a)),
                });
            }
        }
    }
    words
}

/// A phrase the model writes over silence, where the clip heard none: the
/// whole clip near-silent, or no speech inside the segment.
fn invented(text: &str, start: f64, end: f64, speech: &Speech) -> bool {
    if !is_silence_phrase(text) {
        return false;
    }
    let near_silent = speech
        .seconds
        .is_some_and(|s| (0.0..NEAR_SILENT_S).contains(&s));
    let none_here = speech
        .regions
        .as_deref()
        .is_some_and(|regions| speech_inside(regions, start, end) <= 0.0);
    near_silent || none_here
}

#[derive(Debug, Clone)]
enum Content {
    Words { text: String, checked: bool },
    NoSpeech,
    Unintelligible,
}

/// A stretch a person owns: the model's words over `over` give way, and a
/// person's words are shown at `span`.
#[derive(Debug, Clone)]
struct Owned {
    over: Span,
    span: Span,
    content: Content,
    id: EditId,
}

/// The stretches a person owns, latest last. A later act replaces an earlier
/// one whose middle it covers (a person's words have no timings, so they are
/// never cut); acts that only share an edge both stand.
fn person_layer(acts: &[&Edit]) -> Vec<Owned> {
    let mut owned: Vec<Owned> = Vec::new();
    for edit in acts {
        let (over, span, content) = match &edit.act {
            Act::Words {
                span,
                over,
                text,
                checked,
                ..
            } => (
                over.cover(*span),
                *span,
                Content::Words {
                    text: text.as_str().to_owned(),
                    checked: *checked,
                },
            ),
            Act::NoSpeech { span, .. } => (*span, *span, Content::NoSpeech),
            Act::Unintelligible { span, .. } => (*span, *span, Content::Unintelligible),
            _ => continue,
        };
        // Replaced when the new act covers its middle: neighbouring lines whose
        // edges merely touch or overlap a little both stand.
        owned.retain(|o| o.over != over && !over.contains(span_mid(o.over)));
        owned.push(Owned {
            over,
            span,
            content,
            id: edit.id,
        });
    }
    owned
}

/// Who speaks when: the person layer over the diarizer over the prints.
struct Speakers<'a> {
    named: Vec<(Span, Name, EditId)>,
    voices: Vec<(String, Name, EditId)>,
    turns: &'a [VoiceTurn],
    guesses: Vec<(String, String, f64)>,
}

impl<'a> Speakers<'a> {
    fn new(input: &Input<'a>, acts: &[&Edit]) -> Self {
        let mut named = Vec::new();
        let mut voices = Vec::new();
        for edit in acts {
            match &edit.act {
                Act::Speaker { span, name, .. } => named.push((*span, name.clone(), edit.id)),
                Act::Voice { cluster, name, .. } => {
                    voices.push((cluster.clone(), name.clone(), edit.id));
                }
                _ => {}
            }
        }
        let guesses = input
            .voices
            .map(|v| {
                v.prints
                    .iter()
                    .filter_map(|(cluster, print)| {
                        match_one(print, input.enrolled)
                            .map(|g| (cluster.clone(), g.person, g.score))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            named,
            voices,
            turns: input.voices.map_or(&[], |v| v.turns.as_slice()),
            guesses,
        }
    }

    /// The speaker at `at`, given the diarized `cluster` there.
    fn resolve(&self, at: Instant, cluster: Option<&str>) -> Option<Speaker> {
        if let Some((_, name, by)) = self
            .named
            .iter()
            .rev()
            .find(|(span, _, _)| span.contains(at))
        {
            return Some(Speaker::Named {
                name: name.clone(),
                by: *by,
            });
        }
        let cluster = cluster?;
        if let Some((_, name, by)) = self.voices.iter().rev().find(|(c, _, _)| c == cluster) {
            return Some(Speaker::Voice {
                name: name.clone(),
                by: *by,
            });
        }
        if let Some((_, name, score)) = self.guesses.iter().find(|(c, _, _)| c == cluster) {
            return Some(Speaker::Guess {
                name: name.clone(),
                score: *score,
            });
        }
        Some(Speaker::Cluster(cluster.to_owned()))
    }
}

/// The diarized speaker for each word, with short runs folded into a
/// neighbour.
fn clusters(words: &[Word], turns: &[VoiceTurn]) -> Vec<Option<String>> {
    let raw: Vec<Option<String>> = words.iter().map(|w| speaker_at(w.at, turns)).collect();
    let mut runs: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    for (i, who) in raw.into_iter().enumerate() {
        match runs.last_mut() {
            Some((last, members)) if *last == who => members.push(i),
            _ => runs.push((who, vec![i])),
        }
    }
    let length = |members: &[usize]| match (members.first(), members.last()) {
        (Some(&a), Some(&b)) => words[b].span.end().micros() - words[a].span.start().micros(),
        _ => 0,
    };
    while runs.len() > 1 {
        let Some(short) = (0..runs.len())
            .filter(|&i| length(&runs[i].1) < MIN_TURN_US)
            .min_by_key(|&i| length(&runs[i].1))
        else {
            break;
        };
        let left = short.checked_sub(1).map(|i| length(&runs[i].1));
        let right = runs.get(short + 1).map(|r| length(&r.1));
        let into = match (left, right) {
            (Some(l), Some(r)) if l >= r => short - 1,
            (Some(_) | None, Some(_)) => short + 1,
            (Some(_), None) => short - 1,
            (None, None) => break,
        };
        let moved = std::mem::take(&mut runs[short].1);
        if into < short {
            runs[into].1.extend(moved);
        } else {
            let mut joined = moved;
            joined.extend(std::mem::take(&mut runs[into].1));
            runs[into].1 = joined;
        }
        runs.remove(short);
        let mut merged: Vec<(Option<String>, Vec<usize>)> = Vec::new();
        for run in runs {
            match merged.last_mut() {
                Some(last) if last.0 == run.0 => last.1.extend(run.1),
                _ => merged.push(run),
            }
        }
        runs = merged;
    }
    let mut out = vec![None; words.len()];
    for (who, members) in runs {
        for i in members {
            out[i].clone_from(&who);
        }
    }
    out
}

/// The diarized turn containing `at`, else the nearest one by edge distance.
fn speaker_at(at: f64, turns: &[VoiceTurn]) -> Option<String> {
    if let Some(turn) = turns.iter().find(|t| t.start <= at && at <= t.end) {
        return Some(turn.speaker.clone());
    }
    turns
        .iter()
        .min_by(|a, b| {
            let da = (a.start - at).abs().min((a.end - at).abs());
            let db = (b.start - at).abs().min((b.end - at).abs());
            da.total_cmp(&db)
        })
        .map(|t| t.speaker.clone())
}

/// Group the surviving words into lines: a break where the segment or the
/// speaker changes.
fn model_lines(words: &[Word], speakers: &Speakers<'_>, heard: Option<&Heard>) -> Vec<Line> {
    let clusters = clusters(words, speakers.turns);
    let resolved: Vec<Option<Speaker>> = words
        .iter()
        .zip(&clusters)
        .map(|(w, c)| speakers.resolve(w.mid, c.as_deref()))
        .collect();
    let speaker = by_sentence(words, &resolved);
    let language = heard.and_then(|h| h.language.clone());
    let household = language
        .as_deref()
        .is_none_or(|l| crate::Language::from_code(l).is_some());
    let mut lines = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let mut j = i + 1;
        while j < words.len() && words[j].segment == words[i].segment && speaker[j] == speaker[i] {
            j += 1;
        }
        let run = &words[i..j];
        let text = run
            .iter()
            .map(|w| w.text.as_str())
            .collect::<String>()
            .trim()
            .to_owned();
        let Some(span) = Span::new(run[0].span.start(), run[j - i - 1].span.end()) else {
            i = j;
            continue;
        };
        let scored: Vec<f64> = run.iter().filter_map(|w| w.probability).collect();
        let mean = (!scored.is_empty()).then(|| scored.iter().sum::<f64>() / scored.len() as f64);
        let timings: Vec<(f64, f64)> = run
            .iter()
            .map(|w| {
                (
                    w.span.start().micros() as f64 / 1e6,
                    w.span.end().micros() as f64 / 1e6,
                )
            })
            .collect();
        let doubted =
            !household || crate::quality::is_foreign_script(&text) || is_implausibly_slow(&timings);
        if !text.is_empty() {
            lines.push(Line {
                span,
                text,
                by: Author::Model,
                speaker: speaker[i].clone(),
                language: language.clone(),
                confidence: if doubted { Some(0.0) } else { mean },
                checked: false,
            });
        }
        i = j;
    }
    lines
}

/// Each word's speaker, decided per sentence: a sentence goes to whoever
/// speaks most of it, by time. A diarized boundary is approximate to a word or
/// so, so a change inside a sentence is jitter, never a reason to break it.
fn by_sentence(words: &[Word], resolved: &[Option<Speaker>]) -> Vec<Option<Speaker>> {
    let mut out = vec![None; words.len()];
    let mut start = 0;
    for end in 0..words.len() {
        let last = end + 1 == words.len() || words[end + 1].segment != words[end].segment;
        if !(last || ends_sentence(&words[end].text)) {
            continue;
        }
        let mut weight: Vec<(Option<&Speaker>, i64)> = Vec::new();
        for k in start..=end {
            let who = resolved[k].as_ref();
            let length = words[k].span.micros().max(1);
            match weight.iter_mut().find(|(w, _)| *w == who) {
                Some((_, total)) => *total += length,
                None => weight.push((who, length)),
            }
        }
        // `max_by_key` keeps the last of equal maxima; reversed, the first.
        let winner = weight
            .iter()
            .rev()
            .max_by_key(|(_, t)| *t)
            .and_then(|(w, _)| (*w).cloned());
        for slot in &mut out[start..=end] {
            slot.clone_from(&winner);
        }
        start = end + 1;
    }
    out
}

fn ends_sentence(word: &str) -> bool {
    word.trim_end().ends_with(['.', '?', '!', '\u{2026}'])
}

fn span_mid(span: Span) -> Instant {
    Instant::from_micros(span.start().micros() + span.micros() / 2).unwrap_or(span.start())
}
