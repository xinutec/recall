//! The rendered view: render's lines for every clip, kept in `rendered_lines`
//! for the read routes, and rebuilt for a clip when what it is made from
//! changes.
//!
//! What a clip is made from is compared, not announced: each clip records the
//! versions of its inputs (its results' finish times, its speech measurement,
//! the newest act on it and on its session, the voiceprints), and [`refresh`]
//! renders every clip whose versions moved. A writer that forgets to say so
//! cannot leave a clip stale. Live turns show until a transcribed clip covers
//! their start.

use crate::clips::{self, ClipError};
use crate::edit_log::{self, LogError};
use crate::line::{HUMAN_MODEL, LIVE_MODEL, NOBODY_SPOKE, Stage};
use crate::rendering;
use crate::results::ResultError;

/// The model the `asr` shim loads by default (`recall.asr.DEFAULT_MODEL`). The
/// queue carries no model, so this copy must match; a test reads `asr.py`.
pub const SHIM_MODEL: &str = "mlx-community/whisper-large-v3-turbo";
use audiocore::job::Kind;
use rusqlite::Connection;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use transcript::render::{Author, Dropped, Line, Speaker, Why};
use transcript::voice::Voiceprint;
use transcript::{Clip, Instant};

crate::statements! {
    /// Every recording a person can see lines from, newest first so a full
    /// render fills the recent timeline first; the room's derived blocks show
    /// none.
    AUDIO_ROWS: Meaning =
        "SELECT a.id, a.path, a.end_utc FROM audio_segments a
         JOIN sources s ON s.id = a.source_id
         WHERE s.kind <> 'derived' AND a.path IS NOT NULL
         ORDER BY a.start_utc DESC";
    RESULT_VERSIONS: Ingest =
        "SELECT c.id, j.kind, j.done_utc FROM jobs j JOIN clips c ON c.filename = j.filename
         WHERE j.kind IN (?1, ?2) AND j.done_utc IS NOT NULL AND j.result IS NOT NULL";
    SPEECH_VERSIONS: Ingest = "SELECT filename, computed_utc FROM segment_speech";
    CLIP_EDITS: Meaning =
        "SELECT clip_id, max(id) FROM edits WHERE clip_id IS NOT NULL GROUP BY clip_id";
    SOURCE_EDITS: Meaning =
        "SELECT source_id, max(id) FROM edits WHERE source_id IS NOT NULL GROUP BY source_id";
    STORED_INPUTS: Meaning = "SELECT clip_id, inputs FROM rendered_clips";
    OLD_ROWS: Meaning =
        "SELECT id, start_utc, end_utc, hidden_reason IS NOT NULL FROM rendered_lines WHERE clip_id = ?1";
    DROP_CLIP: Meaning = "DELETE FROM rendered_lines WHERE clip_id = ?1";
    INSERT_LINE: Meaning =
        "INSERT INTO rendered_lines (id, clip_id, audio_segment_id, source_id, start_utc, end_utc,
             text, language, asr_confidence, asr_model, speaker_label, speaker_guess, speaker_score,
             speaker_cluster, tier, hidden_reason, words_checked, edit_id, live_id, word_timings)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20)";
    SET_INPUTS: Meaning =
        "INSERT INTO rendered_clips (clip_id, inputs) VALUES (?1, ?2)
         ON CONFLICT (clip_id) DO UPDATE SET inputs = excluded.inputs";
    LIVE_TURNS: Meaning = "SELECT id, start_utc, end_utc, text, language FROM live_lines";
    OLD_LIVE: Meaning = "SELECT live_id, id FROM rendered_lines WHERE live_id IS NOT NULL";
    DROP_LIVE: Meaning = "DELETE FROM rendered_lines WHERE live_id IS NOT NULL";
}

#[derive(Debug)]
pub enum ViewError {
    Db(rusqlite::Error),
    Clip(ClipError),
    Log(LogError),
    /// The clip's stored result does not read.
    Result(ResultError),
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Clip(err) => write!(f, "{err}"),
            Self::Log(err) => write!(f, "{err}"),
            Self::Result(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ViewError {}

impl From<rusqlite::Error> for ViewError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

impl From<ClipError> for ViewError {
    fn from(err: ClipError) -> Self {
        Self::Clip(err)
    }
}

impl From<LogError> for ViewError {
    fn from(err: LogError) -> Self {
        Self::Log(err)
    }
}

/// What a refresh did.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Refreshed {
    pub rendered: usize,
    pub unchanged: usize,
    /// Clips whose stored result does not read, with why; their old lines stay.
    pub unreadable: Vec<(i64, String)>,
    pub live_shown: usize,
}

/// One recording a person can see lines from.
struct Shown {
    clip: Clip,
    audio_id: i64,
    end: Option<Instant>,
}

/// Render every clip whose inputs moved, or every clip with `all`, then the
/// live turns no transcribed clip covers.
///
/// # Errors
/// If a store refuses, or the edit log does not read.
pub fn refresh(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
    all: bool,
) -> Result<Refreshed, ViewError> {
    let shown = shown(meaning, ingest, root)?;
    let prints = crate::identify::enrolled(meaning)?;
    let versions = Versions::read(meaning, ingest, &prints)?;
    let stored: HashMap<i64, String> = STORED_INPUTS
        .prepare(meaning)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut done = Refreshed::default();
    for s in &shown {
        let inputs = versions.of(&s.clip);
        if !all && stored.get(&s.clip.id.stored()) == Some(&inputs) {
            done.unchanged += 1;
            continue;
        }
        match render_one(meaning, ingest, s, &prints, &inputs) {
            Ok(()) => done.rendered += 1,
            Err(Failed::Result(err)) => done.unreadable.push((s.clip.id.stored(), err.to_string())),
            Err(Failed::View(err)) => return Err(err),
        }
    }
    let transcribed: Vec<(Instant, Instant)> = shown
        .iter()
        .filter(|s| versions.transcribed.contains_key(&s.clip.id.stored()))
        .filter_map(|s| Some((s.clip.start, s.end?)))
        .collect();
    done.live_shown = live(meaning, &transcribed)?;
    Ok(done)
}

/// Render these clips now: a person's act shows in the reply that made it.
///
/// # Errors
/// If a store refuses, the edit log or a clip's result does not read.
pub fn render_clips(
    meaning: &Connection,
    ingest: &Connection,
    root: &Path,
    clips: &[transcript::ClipId],
) -> Result<(), ViewError> {
    let prints = crate::identify::enrolled(meaning)?;
    let versions = Versions::read(meaning, ingest, &prints)?;
    for s in shown(meaning, ingest, root)?
        .iter()
        .filter(|s| clips.contains(&s.clip.id))
    {
        match render_one(meaning, ingest, s, &prints, &versions.of(&s.clip)) {
            Ok(()) => {}
            Err(Failed::Result(err)) => return Err(ViewError::Result(err)),
            Err(Failed::View(err)) => return Err(err),
        }
    }
    Ok(())
}

fn shown(meaning: &Connection, ingest: &Connection, root: &Path) -> Result<Vec<Shown>, ViewError> {
    let by_path: HashMap<String, Clip> = clips::all(ingest)?
        .into_iter()
        .map(|c| (c.path.clone(), c))
        .collect();
    let rows: Vec<(i64, String, Option<String>)> = AUDIO_ROWS
        .prepare(meaning)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let prefix = format!("{}/", root.to_string_lossy());
    Ok(rows
        .into_iter()
        .filter_map(|(audio_id, path, end)| {
            let clip = by_path.get(path.strip_prefix(&prefix)?)?.clone();
            Some(Shown {
                clip,
                audio_id,
                end: end.as_deref().and_then(Instant::parse),
            })
        })
        .collect())
}

/// The current version of every input, read once per refresh.
struct Versions {
    transcribed: HashMap<i64, String>,
    diarized: HashMap<i64, String>,
    speech: HashMap<String, String>,
    clip_edits: HashMap<i64, i64>,
    source_edits: HashMap<String, i64>,
    prints: u64,
}

impl Versions {
    fn read(
        meaning: &Connection,
        ingest: &Connection,
        prints: &[Voiceprint],
    ) -> rusqlite::Result<Self> {
        let mut transcribed = HashMap::new();
        let mut diarized = HashMap::new();
        let mut stmt = RESULT_VERSIONS.prepare(ingest)?;
        let rows = stmt.query_map((Kind::TranscribeSegment, Kind::DiarizeSegment), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Kind>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (clip, kind, done) = row?;
            match kind {
                Kind::TranscribeSegment => transcribed.insert(clip, done),
                _ => diarized.insert(clip, done),
            };
        }
        let speech = SPEECH_VERSIONS
            .prepare(ingest)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let clip_edits = CLIP_EDITS
            .prepare(meaning)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let source_edits = SOURCE_EDITS
            .prepare(meaning)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for print in prints {
            print.person.hash(&mut hasher);
            for v in &print.vector {
                v.to_bits().hash(&mut hasher);
            }
        }
        Ok(Self {
            transcribed,
            diarized,
            speech,
            clip_edits,
            source_edits,
            prints: hasher.finish(),
        })
    }

    /// The clip's inputs, spelled out so a stored row can be read by a person.
    fn of(&self, clip: &Clip) -> String {
        let id = clip.id.stored();
        format!(
            "transcribed={} diarized={} speech={} edit={} session-edit={} prints={:x}",
            self.transcribed.get(&id).map_or("-", String::as_str),
            self.diarized.get(&id).map_or("-", String::as_str),
            self.speech.get(&clip.filename).map_or("-", String::as_str),
            self.clip_edits.get(&id).copied().unwrap_or(0),
            self.source_edits
                .get(clip.source.as_str())
                .copied()
                .unwrap_or(0),
            self.prints,
        )
    }
}

enum Failed {
    /// The clip's stored result does not read: its old lines stay.
    Result(ResultError),
    View(ViewError),
}

impl From<rusqlite::Error> for Failed {
    fn from(err: rusqlite::Error) -> Self {
        Self::View(ViewError::Db(err))
    }
}

impl From<LogError> for Failed {
    fn from(err: LogError) -> Self {
        Self::View(ViewError::Log(err))
    }
}

/// Replace the clip's rows with what render makes of it now. A line whose
/// span is unchanged keeps its id, hidden or shown, so an id the app holds
/// stays good.
fn render_one(
    meaning: &Connection,
    ingest: &Connection,
    s: &Shown,
    prints: &[Voiceprint],
    inputs: &str,
) -> Result<(), Failed> {
    let facts = match rendering::facts(ingest, &s.clip) {
        Ok(facts) => facts,
        Err(ResultError::Db(err)) => return Err(err.into()),
        Err(err) => return Err(Failed::Result(err)),
    };
    let edits = edit_log::for_clip(meaning, &s.clip)?;
    let out = rendering::lines(&s.clip, &facts, &edits, prints);
    let tier = if facts.voices.is_some() {
        Stage::Diarized
    } else {
        Stage::Transcribed
    };
    let tx = crate::sql::write_shared(meaning)?;
    let mut old: HashMap<(String, String, bool), i64> = HashMap::new();
    {
        let mut stmt = OLD_ROWS.prepare(&tx)?;
        let rows = stmt.query_map([s.clip.id.stored()], |r| {
            Ok(((r.get(1)?, r.get(2)?, r.get(3)?), r.get(0)?))
        })?;
        for row in rows {
            let (key, id) = row?;
            old.insert(key, id);
        }
    }
    DROP_CLIP.execute(&tx, [s.clip.id.stored()])?;
    let source = s.clip.source.as_str();
    // Same span, same side first; then the same span hidden or shown again.
    let mut kept = |row: &Row, hidden: bool| {
        old.remove(&(row.start.clone(), row.end.clone(), hidden))
            .or_else(|| old.remove(&(row.start.clone(), row.end.clone(), !hidden)))
    };
    for line in &out.lines {
        let row = Row::line(line, tier)?;
        row.insert(&tx, kept(&row, false), Some(s), source)?;
    }
    for dropped in runs(&out.dropped) {
        let row = Row::dropped(&dropped, tier);
        row.insert(&tx, kept(&row, true), Some(s), source)?;
    }
    SET_INPUTS.execute(&tx, (s.clip.id.stored(), inputs))?;
    tx.commit()?;
    Ok(())
}

/// Render leaves words out one by one; the hidden view shows each run of
/// neighbours left out for one reason as one line. Neighbours: at most
/// [`RUN_GAP_US`] apart, the pause between words of one sentence.
fn runs(dropped: &[Dropped]) -> Vec<Dropped> {
    let mut sorted: Vec<&Dropped> = dropped.iter().collect();
    sorted.sort_by_key(|d| (d.span.start(), d.span.end()));
    let mut out: Vec<Dropped> = Vec::new();
    for d in sorted {
        match out.last_mut() {
            Some(last)
                if last.why == d.why
                    && d.span.start().micros() - last.span.end().micros() <= RUN_GAP_US =>
            {
                last.text.push_str(&d.text);
                if let Some(joined) = transcript::Span::new(last.span.start(), d.span.end()) {
                    last.span = joined;
                }
            }
            _ => out.push(d.clone()),
        }
    }
    out
}

const RUN_GAP_US: i64 = 1_000_000;

/// One row of `rendered_lines`, before it is placed.
struct Row {
    start: String,
    end: String,
    text: String,
    language: Option<String>,
    confidence: Option<f64>,
    model: &'static str,
    label: Option<String>,
    guess: Option<String>,
    score: Option<f64>,
    cluster: Option<String>,
    tier: Stage,
    hidden: Option<String>,
    checked: bool,
    edit: Option<i64>,
    live: Option<i64>,
    words: Option<String>,
}

fn spelled(at: Instant) -> String {
    audiocore::instant::python_isoformat_utc(at.to_utc())
}

impl Row {
    fn line(line: &Line, tier: Stage) -> rusqlite::Result<Self> {
        let (label, guess, score) = match line.speaker() {
            Some(Speaker::Named { name, .. } | Speaker::Voice { name, .. }) => {
                (Some(name.as_str().to_owned()), None, None)
            }
            Some(Speaker::Guess { name, score }) => (None, Some(name.clone()), Some(*score)),
            Some(Speaker::Cluster(_)) | None => (None, None, None),
        };
        let (model, tier, edit) = match line.by() {
            Author::Model => (SHIM_MODEL, tier, None),
            Author::Person(id) => (HUMAN_MODEL, Stage::Corrected, Some(id.stored())),
        };
        Ok(Self {
            start: spelled(line.span().start()),
            end: spelled(line.span().end()),
            text: line.text().to_owned(),
            language: line.language().map(str::to_owned),
            confidence: line.confidence(),
            model,
            label,
            guess,
            score,
            cluster: line.cluster().map(str::to_owned),
            tier,
            hidden: None,
            checked: line.checked(),
            edit,
            live: None,
            words: if line.words().is_empty() {
                None
            } else {
                let timed: Vec<(i64, i64, &str)> = line
                    .words()
                    .iter()
                    .map(|(span, w)| (span.start().micros(), span.end().micros(), w.as_str()))
                    .collect();
                Some(
                    serde_json::to_string(&timed)
                        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?,
                )
            },
        })
    }

    fn dropped(dropped: &Dropped, tier: Stage) -> Self {
        let (hidden, edit) = match dropped.why {
            Why::RepetitionLoop => ("a repetition loop".to_owned(), None),
            Why::Wordless => ("no words".to_owned(), None),
            Why::SilencePhrase => ("a phrase written over silence".to_owned(), None),
            Why::Replaced(id) => ("replaced by a person's words".to_owned(), Some(id.stored())),
            Why::NoSpeech(id) => (NOBODY_SPOKE.to_owned(), Some(id.stored())),
            Why::Unintelligible(id) => ("cannot be made out".to_owned(), Some(id.stored())),
            Why::Excluded(id) => ("left out".to_owned(), Some(id.stored())),
        };
        Self {
            start: spelled(dropped.span.start()),
            end: spelled(dropped.span.end()),
            text: dropped.text.trim().to_owned(),
            language: None,
            confidence: None,
            model: SHIM_MODEL,
            label: None,
            guess: None,
            score: None,
            cluster: None,
            tier,
            hidden: Some(hidden),
            checked: false,
            edit,
            live: None,
            words: None,
        }
    }

    fn insert(
        &self,
        conn: &Connection,
        id: Option<i64>,
        shown: Option<&Shown>,
        source: &str,
    ) -> rusqlite::Result<()> {
        INSERT_LINE.execute(
            conn,
            rusqlite::params![
                id,
                shown.map(|s| s.clip.id.stored()),
                shown.map(|s| s.audio_id),
                (!source.is_empty()).then_some(source),
                self.start,
                self.end,
                self.text,
                self.language,
                self.confidence,
                self.model,
                self.label,
                self.guess,
                self.score,
                self.cluster,
                self.tier.as_str(),
                self.hidden,
                self.checked.then_some(1),
                self.edit,
                self.live,
                self.words,
            ],
        )?;
        Ok(())
    }
}

/// Show each live turn no transcribed clip covers; returns how many.
fn live(meaning: &Connection, transcribed: &[(Instant, Instant)]) -> rusqlite::Result<usize> {
    let turns: Vec<(i64, String, String, String, Option<String>)> = LIVE_TURNS
        .prepare(meaning)?
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let tx = crate::sql::write_shared(meaning)?;
    let old: HashMap<i64, i64> = OLD_LIVE
        .prepare(&tx)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    DROP_LIVE.execute(&tx, [])?;
    let mut shown = 0;
    for (live_id, start, end, text, language) in turns {
        let Some(at) = Instant::parse(&start) else {
            continue;
        };
        if transcribed.iter().any(|(a, b)| *a <= at && at < *b) {
            continue;
        }
        Row {
            start,
            end,
            text,
            language,
            confidence: None,
            model: LIVE_MODEL,
            label: None,
            guess: None,
            score: None,
            cluster: None,
            tier: Stage::Live,
            hidden: None,
            checked: false,
            edit: None,
            live: Some(live_id),
            words: None,
        }
        .insert(&tx, old.get(&live_id).copied(), None, "")?;
        shown += 1;
    }
    tx.commit()?;
    Ok(shown)
}
