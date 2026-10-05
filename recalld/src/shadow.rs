//! The shadow diff (#1916): render every clip that has a stored result and
//! compare it with the lines recall shows today. Nothing switches to `render`
//! until every class of difference here is explained.
//!
//! Reports counts and clip ids, never transcript text.

use crate::clips;
use crate::legacy_edits;
use crate::rendering;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::Path;
use transcript::render::{Author, Line, Speaker, Why};
use transcript::{Edit, EditId};

crate::statements! {
    /// Clips with a visible line today, by their audio row.
    SHOWN_CLIPS: Meaning =
        "SELECT DISTINCT a.id, a.path FROM transcript_segments t
         JOIN audio_segments a ON a.id = t.audio_segment_id
         WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL
           AND coalesce(t.asr_model, '') <> 'live'
         ORDER BY a.id";
    AUDIO_FOR: Meaning = "SELECT id FROM audio_segments WHERE path = ?1";
    SHOWN_LINES: Meaning =
        "SELECT text, coalesce(speaker_label, speaker_guess, speaker_cluster),
                CASE WHEN speaker_label IS NOT NULL THEN 'person'
                     WHEN speaker_guess IS NOT NULL THEN 'guess'
                     WHEN speaker_cluster IS NOT NULL THEN 'cluster' ELSE 'none' END,
                start_utc, end_utc, asr_model = 'human'
         FROM transcript_segments
         WHERE audio_segment_id = ?1 AND superseded_by IS NULL AND hidden_reason IS NULL
           AND coalesce(asr_model, '') <> 'live'
         ORDER BY start_utc, id";
}

/// How one clip's rendered lines compare with today's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Class {
    /// Same words, same line breaks, same speakers.
    Same,
    /// Same words and breaks; some line's speaker differs.
    Speakers,
    /// Same words, broken into lines differently.
    Breaks,
    /// The words differ: render drops text today shows, by a rule with a
    /// reason (loop, wordless, silence phrase where no speech was heard).
    WordsRenderDrops,
    /// The words differ: today hides model text that render shows.
    WordsTodayHides,
    /// The words differ both ways, all of them from the current result.
    WordsMixed,
    /// The words differ: today's text is not in the current result, so it
    /// came from an earlier one.
    WordsOtherResult,
    /// Today shows lines; render shows none.
    RenderEmpty,
    /// The clip has no stored transcription: it needs one first.
    NoResult,
    /// A stored result does not parse.
    UnreadableResult,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub clips: BTreeMap<Class, usize>,
    pub lines_today: usize,
    pub lines_rendered: usize,
    /// Over the `Words` class: words today, and words in an edit-distance
    /// alignment that differ.
    pub words_today: usize,
    pub words_differing: usize,
    /// Over the `Speakers` class: each differing line, as
    /// "today's kind -> render's kind" (person, guess, cluster, none).
    pub speaker_changes: BTreeMap<String, usize>,
    /// A few clips for each of those changes.
    pub speaker_change_examples: BTreeMap<String, Vec<i64>>,
    /// Clips where a line a person named shows another name in render.
    pub person_names_changed: Vec<i64>,
    /// Across every class: lines a person named or wrote today, and how many
    /// render shows differently, with clip ids.
    pub person_named_lines: usize,
    pub person_name_differs: usize,
    pub person_written_lines: usize,
    pub person_text_missing: usize,
    pub person_examples: Vec<i64>,
    /// `RenderEmpty` clips whose every word today is in text render dropped
    /// by a rule; the rest, with ids.
    pub empty_explained: usize,
    pub empty_unexplained: Vec<i64>,
    /// A few clip ids per class, to look at.
    pub examples: BTreeMap<Class, Vec<i64>>,
    pub unmapped_audio_rows: usize,
}

/// One line as recall shows it today.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Today {
    pub text: String,
    pub speaker: Option<String>,
    /// Where the name comes from: person, guess, cluster or none.
    pub kind: String,
    pub start: String,
    pub end: String,
    /// A person wrote the words.
    pub human: bool,
}

fn today_lines(meaning: &Connection, audio: i64) -> rusqlite::Result<Vec<Today>> {
    SHOWN_LINES
        .prepare(meaning)?
        .query_map([audio], |r| {
            Ok(Today {
                text: r.get(0)?,
                speaker: r.get(1)?,
                kind: r.get(2)?,
                start: r.get(3)?,
                end: r.get(4)?,
                human: r.get::<_, Option<bool>>(5)?.unwrap_or(false),
            })
        })?
        .collect()
}

/// The edit log as converted from today's tables, numbered in order.
fn edits(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
) -> Result<Vec<Edit>, legacy_edits::LoadError> {
    let loaded = legacy_edits::load(meaning, ingest, root)?;
    let now = transcript::Instant::from_utc(chrono::Utc::now());
    let recovered = legacy_edits::convert(
        &loaded.corrections,
        &loaded.lines,
        &loaded.hides,
        &loaded.pins,
        now,
    );
    Ok(recovered
        .acts
        .into_iter()
        .enumerate()
        .map(|(i, (at, act, _))| Edit {
            id: EditId::from_stored(i64::try_from(i).unwrap_or(i64::MAX) + 1),
            at,
            act,
        })
        .collect())
}

pub fn run(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
) -> Result<Report, Box<dyn std::error::Error>> {
    let edits = edits(meaning, ingest, root)?;
    let enrolled = crate::identify::enrolled(meaning)?;

    let shown: Vec<(i64, String)> = SHOWN_CLIPS
        .prepare(meaning)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut report = Report::default();
    for (audio_id, path) in shown {
        let Some(clip) = clips::for_audio_path(ingest, root, &path)? else {
            report.unmapped_audio_rows += 1;
            continue;
        };
        let today = today_lines(meaning, audio_id)?;
        let class = match rendering::facts(ingest, &clip) {
            Err(crate::results::ResultError::Db(err)) => return Err(err.into()),
            Err(_) => Class::UnreadableResult,
            Ok(facts) => classify(&clip, &facts, &today, &edits, &enrolled, &mut report),
        };
        *report.clips.entry(class).or_default() += 1;
        let examples = report.examples.entry(class).or_default();
        if examples.len() < 12 {
            examples.push(clip.id.stored());
        }
    }
    Ok(report)
}

fn classify(
    clip: &transcript::Clip,
    facts: &rendering::Facts,
    today: &[Today],
    edits: &[Edit],
    enrolled: &[transcript::voice::Voiceprint],
    report: &mut Report,
) -> Class {
    if facts.heard.is_none() {
        return Class::NoResult;
    }
    let out = rendering::lines(clip, facts, edits, enrolled);
    report.lines_today += today.len();
    report.lines_rendered += out.lines.len();
    person_check(clip.id.stored(), today, &out.lines, report);
    if out.lines.is_empty() {
        let dropped: std::collections::BTreeSet<String> = out
            .dropped
            .iter()
            .flat_map(|d| tokens(&d.text).collect::<Vec<_>>())
            .collect();
        if today
            .iter()
            .all(|t| tokens(&t.text).all(|w| dropped.contains(&w)))
        {
            report.empty_explained += 1;
        } else {
            report.empty_unexplained.push(clip.id.stored());
        }
    }
    let class = compare(clip.id.stored(), today, &out.lines, report);
    if class != Class::WordsRenderDrops {
        return class;
    }
    // Which way do the words differ?
    let bag = |texts: &mut dyn Iterator<Item = &str>| -> BTreeMap<String, i64> {
        let mut bag = BTreeMap::new();
        for t in texts.flat_map(tokens) {
            *bag.entry(t).or_insert(0) += 1;
        }
        bag
    };
    let today_bag = bag(&mut today.iter().map(|t| t.text.as_str()));
    let rendered_bag = bag(&mut out.lines.iter().map(Line::text));
    let ruled = bag(&mut out
        .dropped
        .iter()
        .filter(|d| {
            matches!(
                d.why,
                Why::RepetitionLoop | Why::Wordless | Why::SilencePhrase
            )
        })
        .map(|d| d.text.as_str()));
    let heard = bag(&mut facts
        .heard
        .iter()
        .flat_map(|h| h.segments.iter().map(|s| s.text.as_str())));
    let person = bag(&mut out
        .lines
        .iter()
        .filter(|l| l.by() != Author::Model)
        .map(Line::text));
    let minus = |a: &BTreeMap<String, i64>, b: &BTreeMap<String, i64>| -> BTreeMap<String, i64> {
        a.iter()
            .filter_map(|(k, n)| {
                let left = n - b.get(k).copied().unwrap_or(0);
                (left > 0).then(|| (k.clone(), left))
            })
            .collect()
    };
    let only_today = minus(&today_bag, &rendered_bag);
    let only_rendered = minus(&rendered_bag, &today_bag);
    let from_result = |extra: &BTreeMap<String, i64>| {
        minus(extra, &heard).is_empty() || minus(&minus(extra, &heard), &person).is_empty()
    };
    if !from_result(&only_today) {
        Class::WordsOtherResult
    } else if only_rendered.is_empty() && minus(&only_today, &ruled).is_empty() {
        Class::WordsRenderDrops
    } else if only_today.is_empty() {
        Class::WordsTodayHides
    } else {
        Class::WordsMixed
    }
}

fn compare(clip: i64, today: &[Today], rendered: &[Line], report: &mut Report) -> Class {
    if rendered.is_empty() {
        return Class::RenderEmpty;
    }
    let words_of =
        |texts: &mut dyn Iterator<Item = &str>| -> Vec<String> { texts.flat_map(tokens).collect() };
    let a = words_of(&mut today.iter().map(|t| t.text.as_str()));
    let b = words_of(&mut rendered.iter().map(Line::text));
    if a != b {
        report.words_today += a.len();
        report.words_differing += distance(&a, &b);
        return Class::WordsRenderDrops;
    }
    let breaks_a: Vec<Vec<String>> = today.iter().map(|t| tokens(&t.text).collect()).collect();
    let breaks_b: Vec<Vec<String>> = rendered
        .iter()
        .map(|l| tokens(l.text()).collect())
        .collect();
    if breaks_a != breaks_b {
        return Class::Breaks;
    }
    let mut same = true;
    let mut person_changed = false;
    for (t, line) in today.iter().zip(rendered) {
        let theirs = line.speaker().map(name);
        if t.speaker.as_deref() == theirs {
            continue;
        }
        same = false;
        let kind = match line.speaker() {
            Some(Speaker::Named { .. } | Speaker::Voice { .. }) => "person",
            Some(Speaker::Guess { .. }) => "guess",
            Some(Speaker::Cluster(_)) => "cluster",
            None => "none",
        };
        let change = format!("{} -> {kind}", t.kind);
        let examples = report
            .speaker_change_examples
            .entry(change.clone())
            .or_default();
        if examples.len() < 12 && !examples.contains(&clip) {
            examples.push(clip);
        }
        *report.speaker_changes.entry(change).or_default() += 1;
        person_changed |= t.kind == "person";
    }
    if person_changed && report.person_names_changed.len() < 40 {
        report.person_names_changed.push(clip);
    }
    if same { Class::Same } else { Class::Speakers }
}

/// Whatever the class, a person's names and words must come through: each
/// named line shows that name at its midpoint, and each written line appears
/// as a person's line with the same words.
fn person_check(clip: i64, today: &[Today], rendered: &[Line], report: &mut Report) {
    let mut wrong = false;
    for t in today {
        let (Some(start), Some(end)) = (
            transcript::Instant::parse(&t.start),
            transcript::Instant::parse(&t.end),
        ) else {
            continue;
        };
        let mid =
            transcript::Instant::from_micros(start.micros() + (end.micros() - start.micros()) / 2)
                .unwrap_or(start);
        if t.kind == "person" {
            report.person_named_lines += 1;
            // The rendered line that shares the most time with it, not the
            // first that brushes its middle: a neighbour's last word can.
            let shown = transcript::Span::new(start, end).and_then(|line| {
                rendered
                    .iter()
                    .filter_map(|l| {
                        l.span()
                            .intersection(line)
                            .map(|shared| (shared.micros(), l))
                    })
                    .max_by_key(|(shared, _)| *shared)
                    .map(|(_, l)| l)
                    .or_else(|| rendered.iter().find(|l| l.span().start() == mid))
            });
            if shown.and_then(|l| l.speaker().map(name)) != t.speaker.as_deref() {
                report.person_name_differs += 1;
                wrong = true;
            }
        }
        if t.human {
            report.person_written_lines += 1;
            let words: Vec<String> = tokens(&t.text).collect();
            let found = rendered
                .iter()
                .any(|l| l.by() != Author::Model && tokens(l.text()).collect::<Vec<_>>() == words);
            if !found {
                report.person_text_missing += 1;
                wrong = true;
            }
        }
    }
    if wrong && report.person_examples.len() < 40 {
        report.person_examples.push(clip);
    }
}

fn name(speaker: &Speaker) -> &str {
    match speaker {
        Speaker::Named { name, .. } | Speaker::Voice { name, .. } => name.as_str(),
        Speaker::Guess { name, .. } | Speaker::Cluster(name) => name,
    }
}

/// Lowercased letter-and-digit runs: what two transcripts must agree on.
fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
}

/// Word-level edit distance.
fn distance(a: &[String], b: &[String]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(x != y))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        previous = current;
    }
    previous[b.len()]
}

/// One clip side by side, for a person looking into a class. Holds transcript
/// text: for inspection on a copy, never for a report.
#[derive(Debug, serde::Serialize)]
pub struct Detail {
    pub today: Vec<Today>,
    pub rendered: Vec<(String, Option<String>, String)>,
    pub dropped: Vec<(String, String)>,
    pub heard_segments: Option<usize>,
    pub voices: Option<usize>,
    /// The acts that touch this clip: id, seconds into the clip, and what.
    pub acts: Vec<(i64, f64, f64, String)>,
}

pub fn detail(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
    clip: i64,
) -> Result<Detail, Box<dyn std::error::Error>> {
    let edits = edits(meaning, ingest, root)?;
    let enrolled = crate::identify::enrolled(meaning)?;
    let clip =
        clips::by_id(ingest, transcript::ClipId::from_stored(clip))?.ok_or("no such clip")?;
    let path = root.join(&clip.path);
    let audio: i64 = AUDIO_FOR.query_row(meaning, [path.to_string_lossy()], |r| r.get(0))?;
    let today = today_lines(meaning, audio)?;
    let facts = rendering::facts(ingest, &clip)?;
    let out = rendering::lines(&clip, &facts, &edits, &enrolled);
    Ok(Detail {
        today,
        rendered: out
            .lines
            .iter()
            .map(|l| {
                (
                    l.text().to_owned(),
                    l.speaker().map(|s| name(s).to_owned()),
                    format!("{:?}", l.by()),
                )
            })
            .collect(),
        dropped: out
            .dropped
            .iter()
            .map(|d| (d.text.clone(), format!("{:?}", d.why)))
            .collect(),
        acts: edits
            .iter()
            .filter_map(|e| {
                let (span, what) = match &e.act {
                    transcript::Act::Words {
                        clip: c,
                        span,
                        text,
                        ..
                    } if *c == clip.id => (span, format!("words {}", text.as_str())),
                    transcript::Act::Speaker {
                        clip: c,
                        span,
                        name,
                        ..
                    } if *c == clip.id => (span, format!("speaker {}", name.as_str())),
                    transcript::Act::NoSpeech { clip: c, span } if *c == clip.id => {
                        (span, "no-speech".to_owned())
                    }
                    _ => return None,
                };
                let at = |i: transcript::Instant| (i.micros() - clip.start.micros()) as f64 / 1e6;
                Some((e.id.stored(), at(span.start()), at(span.end()), what))
            })
            .collect(),
        heard_segments: facts.heard.as_ref().map(|h| h.segments.len()),
        voices: facts.voices.as_ref().map(|v| v.turns.len()),
    })
}
