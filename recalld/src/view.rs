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
use crate::line::{HUMAN_MODEL, HiddenReason, LIVE_MODEL, Stage, WordTiming};
use crate::rendering;
use crate::results::ResultError;
use audiocore::instant::Stamp;

/// The model the `asr` shim loads by default (`recall.asr.DEFAULT_MODEL`). The
/// queue carries no model, so this copy must match; a test reads `asr.py`.
pub const SHIM_MODEL: &str = "mlx-community/whisper-large-v3-turbo";
use audiocore::job::Kind;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;
use transcript::render::{Author, Dropped, Line, Speaker, Why};
use transcript::voice::Voiceprint;
use transcript::{Clip, EditId, Instant};

/// `rendered_lines`' columns, in [`StoredLine`]'s order after the id.
macro_rules! line_columns {
    () => {
        "id, clip_id, audio_segment_id, source_id, start_utc, end_utc, text, language,
         asr_confidence, asr_model, speaker_label, speaker_guess, speaker_score,
         speaker_cluster, tier, hidden_reason, words_checked, edit_id, live_id, word_timings"
    };
}

crate::statements! {
    /// Every recording a person can see lines from, newest first so a full
    /// render fills the recent timeline first; the room's derived blocks show
    /// none.
    AUDIO_ROWS: Meaning =
        "SELECT a.id, a.path, a.end_utc FROM audio_segments a
         JOIN sources s ON s.id = a.source_id
         WHERE s.kind <> ?1 AND a.path IS NOT NULL
         ORDER BY a.start_utc DESC";
    RESULT_VERSIONS: Ingest =
        "SELECT filename, kind, done_utc FROM jobs
         WHERE kind IN (?1, ?2) AND done_utc IS NOT NULL AND result IS NOT NULL";
    SPEECH_VERSIONS: Ingest = "SELECT filename, computed_utc FROM segment_speech";
    CLIP_EDITS: Meaning =
        "SELECT clip_id, max(id) FROM edits WHERE clip_id IS NOT NULL GROUP BY clip_id";
    SOURCE_EDITS: Meaning =
        "SELECT source_id, max(id) FROM edits WHERE source_id IS NOT NULL GROUP BY source_id";
    STORED_INPUTS: Meaning = "SELECT clip_id, inputs FROM rendered_clips";
    OLD_ROWS: Meaning =
        "SELECT ", line_columns!(), " FROM rendered_lines WHERE clip_id = ?1 ORDER BY id";
    DROP_CLIP: Meaning = "DELETE FROM rendered_lines WHERE clip_id = ?1";
    INSERT_LINE: Meaning =
        "INSERT INTO rendered_lines (", line_columns!(), ")
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 ?19, ?20)";
    SET_INPUTS: Meaning =
        "INSERT INTO rendered_clips (clip_id, inputs) VALUES (?1, ?2)
         ON CONFLICT (clip_id) DO UPDATE SET inputs = excluded.inputs";
    LIVE_TURNS: Meaning = "SELECT id, start_utc, end_utc, text, language FROM live_lines";
    /// As [`OLD_ROWS`], for the live lines.
    OLD_LIVE: Meaning =
        "SELECT ", line_columns!(), " FROM rendered_lines WHERE live_id IS NOT NULL ORDER BY id";
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
    /// Clips rendered and compared with their stored lines.
    pub rendered: usize,
    /// Of those, the clips whose lines changed and were written.
    pub written: usize,
    pub unchanged: usize,
    /// Clips whose stored result does not read, with why; their old lines stay.
    pub unreadable: Vec<(i64, String)>,
    /// Clips whose rows would not write, with why: their old lines stay, and
    /// the next pass tries them again.
    pub failed: Vec<(i64, String)>,
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
    let recovered = edit_log::recovered(meaning)?;
    let versions = Versions::read(meaning, ingest, &prints)?;
    let stored: HashMap<i64, String> = STORED_INPUTS
        .prepare(meaning)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut done = Refreshed::default();
    let mut due = Vec::new();
    for s in &shown {
        let inputs = versions.of(&s.clip);
        if !all && stored.get(&s.clip.id.stored()) == Some(&inputs) {
            done.unchanged += 1;
        } else {
            due.push((s, inputs));
        }
    }
    for batch in due.chunks(BATCH) {
        // Read and render before taking the write lock: a cold result is slow.
        let mut ready = Vec::new();
        for (s, inputs) in batch {
            match prepare(meaning, ingest, s, &prints, &recovered) {
                Ok(rows) => ready.push((*s, inputs, rows)),
                Err(Failed::Result(err)) => {
                    done.unreadable.push((s.clip.id.stored(), err.to_string()));
                }
                Err(Failed::View(err)) => done.failed.push((s.clip.id.stored(), err.to_string())),
            }
        }
        let mut tx = crate::sql::write_shared(meaning)?;
        let mut wrote = false;
        for (s, inputs, rows) in &ready {
            let known = stored.get(&s.clip.id.stored()).map(String::as_str);
            match write_clip(&mut tx, s, rows, inputs, known) {
                Ok(what) => {
                    done.rendered += 1;
                    done.written += usize::from(what == Wrote::Lines);
                    wrote |= what != Wrote::Nothing;
                }
                // One clip must not hold up the rest: its savepoint rolled back.
                Err(err) => done.failed.push((s.clip.id.stored(), err.to_string())),
            }
        }
        // Nothing to keep: no commit, so no fsync.
        if wrote {
            tx.commit()?;
        } else {
            tx.rollback()?;
        }
    }
    let transcribed: Vec<(Instant, Instant)> = shown
        .iter()
        .filter(|s| versions.transcribed.contains_key(&s.clip.filename))
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
    let recovered = edit_log::recovered(meaning)?;
    let versions = Versions::read(meaning, ingest, &prints)?;
    for s in shown(meaning, ingest, root)?
        .iter()
        .filter(|s| clips.contains(&s.clip.id))
    {
        let rows = match prepare(meaning, ingest, s, &prints, &recovered) {
            Ok(rows) => rows,
            Err(Failed::Result(err)) => return Err(ViewError::Result(err)),
            Err(Failed::View(err)) => return Err(err),
        };
        let mut tx = crate::sql::write_shared(meaning)?;
        write_clip(&mut tx, s, &rows, &versions.of(&s.clip), None)?;
        tx.commit()?;
    }
    Ok(())
}

fn shown(meaning: &Connection, ingest: &Connection, root: &Path) -> Result<Vec<Shown>, ViewError> {
    let by_path = clips::by_audio_path(ingest, root)?;
    let rows: Vec<(i64, String, Option<String>)> = AUDIO_ROWS
        .prepare(meaning)?
        .query_map([crate::sources::SourceKind::Derived], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(audio_id, path, end)| {
            let clip = by_path.get(&path)?.clone();
            Some(Shown {
                clip,
                audio_id,
                end: end.as_deref().and_then(Instant::parse),
            })
        })
        .collect())
}

/// The current version of every input, read once per refresh.
/// Model results and speech by the clip's stored filename, as their jobs name it.
struct Versions {
    transcribed: HashMap<String, String>,
    diarized: HashMap<String, String>,
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
                r.get::<_, String>(0)?,
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
            self.transcribed
                .get(&clip.filename)
                .map_or("-", String::as_str),
            self.diarized
                .get(&clip.filename)
                .map_or("-", String::as_str),
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

/// Clips per transaction. A commit is an fsync: one per clip was 16 minutes
/// on Isis's disk for a full render, a batch of this many about half a second.
const BATCH: usize = 200;

/// What [`write_clip`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Wrote {
    Nothing,
    /// Only what the clip was rendered from.
    Inputs,
    Lines,
}

/// The rows render makes of the clip now, read and rendered without a lock.
fn prepare(
    meaning: &Connection,
    ingest: &Connection,
    s: &Shown,
    prints: &[Voiceprint],
    recovered: &HashSet<EditId>,
) -> Result<Vec<Row>, Failed> {
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
    let mut rows = Vec::new();
    for line in &out.lines {
        rows.push(Row::line(line, tier)?);
    }
    rows.extend(
        runs(&out.dropped)
            .iter()
            .map(|d| Row::dropped(d, tier, recovered)),
    );
    Ok(rows)
}

/// Make the clip's stored rows `rows`, under a savepoint of `tx`: a failure
/// undoes this clip only. A line whose span is unchanged keeps its id, hidden
/// or shown, so an id the app holds stays good; rows that would come out the
/// same are not written at all. `known`: the inputs stored for the clip.
fn write_clip(
    tx: &mut rusqlite::Transaction<'_>,
    s: &Shown,
    rows: &[Row],
    inputs: &str,
    known: Option<&str>,
) -> rusqlite::Result<Wrote> {
    let sp = tx.savepoint()?;
    let mut old: Vec<(i64, StoredLine)> = Vec::new();
    {
        let mut stmt = OLD_ROWS.prepare(&sp)?;
        let found = stmt.query_map([s.clip.id.stored()], stored_row)?;
        for row in found {
            old.push(row?);
        }
    }
    let source = s.clip.source.as_str();
    let new = with_ids(rows, &old, Some(s), source);
    let wrote = if same(&old, &new) {
        Wrote::Nothing
    } else {
        DROP_CLIP.execute(&sp, [s.clip.id.stored()])?;
        for (id, line) in &new {
            insert(&sp, *id, line)?;
        }
        Wrote::Lines
    };
    let wrote = if wrote == Wrote::Nothing && known == Some(inputs) {
        Wrote::Nothing
    } else {
        SET_INPUTS.execute(&sp, (s.clip.id.stored(), inputs))?;
        wrote.max(Wrote::Inputs)
    };
    sp.commit()?;
    Ok(wrote)
}

/// A `rendered_lines` row after its id, column for column: what is written,
/// and what a stored row is compared with, so equal means stored unchanged.
#[derive(Debug, Clone, PartialEq)]
struct StoredLine {
    clip_id: Option<i64>,
    audio_segment_id: Option<i64>,
    source_id: Option<String>,
    start_utc: String,
    end_utc: String,
    text: String,
    language: Option<String>,
    asr_confidence: Option<f64>,
    asr_model: Option<String>,
    speaker_label: Option<String>,
    speaker_guess: Option<String>,
    speaker_score: Option<f64>,
    speaker_cluster: Option<String>,
    tier: String,
    hidden_reason: Option<String>,
    words_checked: Option<i64>,
    edit_id: Option<i64>,
    live_id: Option<i64>,
    word_timings: Option<String>,
}

/// A stored row: its id, and its [`StoredLine`].
fn stored_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, StoredLine)> {
    Ok((
        r.get("id")?,
        StoredLine {
            clip_id: r.get("clip_id")?,
            audio_segment_id: r.get("audio_segment_id")?,
            source_id: r.get("source_id")?,
            start_utc: r.get("start_utc")?,
            end_utc: r.get("end_utc")?,
            text: r.get("text")?,
            language: r.get("language")?,
            asr_confidence: r.get("asr_confidence")?,
            asr_model: r.get("asr_model")?,
            speaker_label: r.get("speaker_label")?,
            speaker_guess: r.get("speaker_guess")?,
            speaker_score: r.get("speaker_score")?,
            speaker_cluster: r.get("speaker_cluster")?,
            tier: r.get("tier")?,
            hidden_reason: r.get("hidden_reason")?,
            words_checked: r.get("words_checked")?,
            edit_id: r.get("edit_id")?,
            live_id: r.get("live_id")?,
            word_timings: r.get("word_timings")?,
        },
    ))
}

/// The new rows with the ids they keep: same span and still hidden or still
/// shown first; then the same span hidden or shown again. A new span gets none. Lines sharing a span
/// (two reasons over one instant) take its ids in order, so each gets its own
/// back.
fn with_ids(
    rows: &[Row],
    old: &[(i64, StoredLine)],
    shown: Option<&Shown>,
    source: &str,
) -> Vec<(Option<i64>, StoredLine)> {
    let key = |l: &StoredLine| {
        (
            l.start_utc.clone(),
            l.end_utc.clone(),
            l.hidden_reason.is_some(),
        )
    };
    let mut free: HashMap<(String, String, bool), std::collections::VecDeque<i64>> = HashMap::new();
    for (id, line) in old {
        free.entry(key(line)).or_default().push_back(*id);
    }
    let mut take = |k: (String, String, bool)| {
        free.get_mut(&k)
            .and_then(std::collections::VecDeque::pop_front)
    };
    rows.iter()
        .map(|row| {
            let line = row.stored(shown, source);
            let (start, end, hidden) = key(&line);
            let id =
                take((start.clone(), end.clone(), hidden)).or_else(|| take((start, end, !hidden)));
            (id, line)
        })
        .collect()
}

/// Whether the new rows are the stored ones, ids and all.
fn same(old: &[(i64, StoredLine)], new: &[(Option<i64>, StoredLine)]) -> bool {
    let by_id: HashMap<i64, &StoredLine> = old.iter().map(|(id, l)| (*id, l)).collect();
    old.len() == new.len()
        && new
            .iter()
            .all(|(id, line)| id.and_then(|id| by_id.get(&id)) == Some(&line))
}

fn insert(conn: &Connection, id: Option<i64>, l: &StoredLine) -> rusqlite::Result<()> {
    INSERT_LINE.execute(
        conn,
        rusqlite::params![
            id,
            l.clip_id,
            l.audio_segment_id,
            l.source_id,
            l.start_utc,
            l.end_utc,
            l.text,
            l.language,
            l.asr_confidence,
            l.asr_model,
            l.speaker_label,
            l.speaker_guess,
            l.speaker_score,
            l.speaker_cluster,
            l.tier,
            l.hidden_reason,
            l.words_checked,
            l.edit_id,
            l.live_id,
            l.word_timings,
        ],
    )?;
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
    start: Stamp,
    end: Stamp,
    text: String,
    language: Option<String>,
    confidence: Option<f64>,
    model: &'static str,
    label: Option<String>,
    guess: Option<String>,
    score: Option<f64>,
    cluster: Option<String>,
    tier: Stage,
    hidden: Option<HiddenReason>,
    checked: bool,
    edit: Option<i64>,
    live: Option<i64>,
    words: Option<String>,
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
            start: line.span().start().into(),
            end: line.span().end().into(),
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
                let timed: Vec<WordTiming> = line
                    .words()
                    .iter()
                    .map(|(span, w)| WordTiming {
                        start_us: span.start().micros(),
                        end_us: span.end().micros(),
                        text: w.clone(),
                    })
                    .collect();
                Some(
                    serde_json::to_string(&timed)
                        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?,
                )
            },
        })
    }

    /// `recovered`: acts recovered from the old tables; their exclusions read
    /// "left out".
    fn dropped(dropped: &Dropped, tier: Stage, recovered: &HashSet<EditId>) -> Self {
        let (hidden, edit) = match dropped.why {
            Why::RepetitionLoop => (HiddenReason::RepetitionLoop, None),
            Why::Wordless => (HiddenReason::NoWords, None),
            Why::SilencePhrase => (HiddenReason::SilencePhrase, None),
            Why::Replaced(id) => (HiddenReason::Replaced, Some(id.stored())),
            Why::NoSpeech(id) => (HiddenReason::NobodySpoke, Some(id.stored())),
            Why::Unintelligible(id) => (HiddenReason::CannotBeMadeOut, Some(id.stored())),
            Why::Excluded(id) if recovered.contains(&id) => {
                (HiddenReason::LeftOut, Some(id.stored()))
            }
            Why::Excluded(id) => (HiddenReason::Excluded, Some(id.stored())),
        };
        Self {
            start: dropped.span.start().into(),
            end: dropped.span.end().into(),
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

    /// The row as stored.
    fn stored(&self, shown: Option<&Shown>, source: &str) -> StoredLine {
        StoredLine {
            clip_id: shown.map(|s| s.clip.id.stored()),
            audio_segment_id: shown.map(|s| s.audio_id),
            source_id: (!source.is_empty()).then(|| source.to_owned()),
            start_utc: self.start.to_string(),
            end_utc: self.end.to_string(),
            text: self.text.clone(),
            language: self.language.clone(),
            asr_confidence: self.confidence,
            asr_model: Some(self.model.to_owned()),
            speaker_label: self.label.clone(),
            speaker_guess: self.guess.clone(),
            speaker_score: self.score,
            speaker_cluster: self.cluster.clone(),
            tier: self.tier.as_str().to_owned(),
            hidden_reason: self.hidden.map(|h| h.as_str().to_owned()),
            words_checked: self.checked.then_some(1),
            edit_id: self.edit,
            live_id: self.live,
            word_timings: self.words.clone(),
        }
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
    let old: Vec<(i64, StoredLine)> = OLD_LIVE
        .prepare(&tx)?
        .query_map([], stored_row)?
        .collect::<rusqlite::Result<_>>()?;
    let by_turn: HashMap<i64, i64> = old
        .iter()
        .filter_map(|(id, line)| line.live_id.map(|turn| (turn, *id)))
        .collect();
    let mut new = Vec::new();
    for (live_id, start, end, text, language) in turns {
        let (Some(at), Some(end)) = (Instant::parse(&start), Stamp::parse(&end)) else {
            continue;
        };
        if transcribed.iter().any(|(a, b)| *a <= at && at < *b) {
            continue;
        }
        let row = Row {
            start: at.into(),
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
        };
        new.push((by_turn.get(&live_id).copied(), row.stored(None, "")));
    }
    // Unchanged, as on almost every pass: no write, no commit.
    if same(&old, &new) {
        tx.rollback()?;
        return Ok(new.len());
    }
    DROP_LIVE.execute(&tx, [])?;
    for (id, line) in &new {
        insert(&tx, *id, line)?;
    }
    tx.commit()?;
    Ok(new.len())
}
