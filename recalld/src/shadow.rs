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
    /// Every voiceprint, with the clip it was cut from.
    PRINTS: Meaning =
        "SELECT s.name, e.vector, coalesce(t.audio_segment_id, c.audio_segment_id)
         FROM speaker_embeddings e JOIN speakers s ON s.id = e.speaker_id
         LEFT JOIN transcript_segments t ON t.id = e.source_segment_id
         LEFT JOIN corrections c ON c.id = e.source_correction_id";
    /// A clip's visible lines a person named, and whether each has its own
    /// voice embedding.
    NAMED_LINES: Meaning =
        "SELECT t.speaker_label, t.start_utc, t.end_utc, e.segment_id IS NOT NULL
         FROM transcript_segments t
         LEFT JOIN transcript_embeddings e ON e.segment_id = t.id
         WHERE t.audio_segment_id = ?1 AND t.superseded_by IS NULL
           AND t.hidden_reason IS NULL AND coalesce(t.asr_model, '') <> 'live'
           AND t.speaker_label IS NOT NULL";
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
    /// Over every rendered clip, whatever its class, split by whether it has a
    /// diarization result: clips, and seconds of line time by speaker kind,
    /// today and rendered. Says what a switch would do to the speakers.
    pub speaker_seconds: BTreeMap<&'static str, SpeakerSeconds>,
    pub unmapped_audio_rows: usize,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct SpeakerSeconds {
    pub clips: usize,
    pub today: BTreeMap<String, f64>,
    pub rendered: BTreeMap<String, f64>,
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

/// The paths of the audio rows that show lines today.
///
/// # Errors
/// If the meaning plane refuses.
pub fn shown_paths(meaning: &Connection) -> rusqlite::Result<Vec<String>> {
    SHOWN_CLIPS
        .prepare(meaning)?
        .query_map([], |r| r.get(1))?
        .collect()
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
    speaker_seconds(facts, today, &out.lines, report);
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

fn speaker_seconds(
    facts: &rendering::Facts,
    today: &[Today],
    rendered: &[Line],
    report: &mut Report,
) {
    let bucket = if facts.voices.is_some() {
        "diarized"
    } else {
        "not diarized"
    };
    let entry = report.speaker_seconds.entry(bucket).or_default();
    entry.clips += 1;
    for t in today {
        if let (Some(start), Some(end)) = (
            transcript::Instant::parse(&t.start),
            transcript::Instant::parse(&t.end),
        ) {
            let seconds = (end.micros() - start.micros()) as f64 / 1e6;
            *entry.today.entry(t.kind.clone()).or_default() += seconds;
        }
    }
    for line in rendered {
        let kind = match line.speaker() {
            Some(Speaker::Named { .. } | Speaker::Voice { .. }) => "person",
            Some(Speaker::Guess { .. }) => "guess",
            Some(Speaker::Cluster(_)) => "cluster",
            None => "none",
        };
        let seconds = line.span().micros() as f64 / 1e6;
        *entry.rendered.entry(kind.to_owned()).or_default() += seconds;
    }
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

/// How render names the lines a person named, if nobody had: each clip
/// rendered without any naming edit and without the voiceprints cut from that
/// clip, so a line is never matched against itself. Split by whether the line
/// has its own embedding: the lines without one are those today shows unnamed
/// and render names from the diarization's per-voice prints.
#[derive(Debug, Default, serde::Serialize)]
pub struct Naming {
    pub clips: usize,
    /// Named lines in clips with no diarization result: render has no voices.
    pub undiarized_lines: usize,
    pub buckets: BTreeMap<&'static str, NamingCounts>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct NamingCounts {
    pub lines: usize,
    pub right: usize,
    /// Another person's name.
    pub wrong: usize,
    /// Shown as an unnamed voice, or with no speaker.
    pub unnamed: usize,
    /// The named person has no voiceprint outside this clip, so no guess can
    /// be right: a stranger's case.
    pub no_print_left: usize,
    pub wrong_examples: Vec<i64>,
}

/// See [`Naming`].
///
/// # Errors
/// If a store refuses or the edit log does not load.
pub fn naming(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
) -> Result<Naming, Box<dyn std::error::Error>> {
    let edits: Vec<Edit> = edits(meaning, ingest, root)?
        .into_iter()
        .filter(|e| {
            !matches!(
                e.act,
                transcript::Act::Speaker { .. } | transcript::Act::Voice { .. }
            )
        })
        .collect();
    let prints: Vec<(transcript::voice::Voiceprint, Option<i64>)> = PRINTS
        .prepare(meaning)?
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get(2)?))
        })?
        .filter_map(|row| {
            let (person, raw, clip) = row.ok()?;
            let vector = serde_json::from_str::<Vec<f64>>(&raw).ok()?;
            Some((transcript::voice::Voiceprint { person, vector }, clip))
        })
        .collect();
    let shown: Vec<(i64, String)> = SHOWN_CLIPS
        .prepare(meaning)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut report = Naming::default();
    for (audio_id, path) in shown {
        let named: Vec<(String, String, String, bool)> = NAMED_LINES
            .prepare(meaning)?
            .query_map([audio_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if named.is_empty() {
            continue;
        }
        let Some(clip) = clips::for_audio_path(ingest, root, &path)? else {
            continue;
        };
        let facts = match rendering::facts(ingest, &clip) {
            Err(crate::results::ResultError::Db(err)) => return Err(err.into()),
            Err(_) => continue,
            Ok(facts) => facts,
        };
        if facts.voices.is_none() || facts.heard.is_none() {
            report.undiarized_lines += named.len();
            continue;
        }
        let enrolled: Vec<_> = prints
            .iter()
            .filter(|(_, from)| *from != Some(audio_id))
            .map(|(print, _)| print.clone())
            .collect();
        let out = rendering::lines(&clip, &facts, &edits, &enrolled);
        report.clips += 1;
        for (label, start, end, embedded) in named {
            let bucket = if embedded {
                "own embedding"
            } else {
                "no own embedding"
            };
            let counts = report.buckets.entry(bucket).or_default();
            counts.lines += 1;
            if !enrolled.iter().any(|p| p.person == label) {
                counts.no_print_left += 1;
                continue;
            }
            let shown = transcript::Instant::parse(&start)
                .zip(transcript::Instant::parse(&end))
                .and_then(|(a, b)| transcript::Span::new(a, b))
                .and_then(|line| {
                    out.lines
                        .iter()
                        .filter_map(|l| l.span().intersection(line).map(|s| (s.micros(), l)))
                        .max_by_key(|(shared, _)| *shared)
                        .map(|(_, l)| l)
                });
            match shown.and_then(Line::speaker) {
                Some(Speaker::Guess { name, .. }) if *name == label => counts.right += 1,
                Some(Speaker::Guess { .. }) => {
                    counts.wrong += 1;
                    if counts.wrong_examples.len() < 12
                        && !counts.wrong_examples.contains(&clip.id.stored())
                    {
                        counts.wrong_examples.push(clip.id.stored());
                    }
                }
                _ => counts.unnamed += 1,
            }
        }
    }
    Ok(report)
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
