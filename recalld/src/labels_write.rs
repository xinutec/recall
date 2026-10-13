//! The labelling writes. Each appends what the person did to the edit log
//! (`edit_log`) and renders the clip again (`view`) before replying, so the
//! reply's view already shows it. Nothing here changes a line: lines are
//! render's.

use crate::clips;
use crate::edit_log::{self, LogError};
use crate::route;
use crate::view::{self, ViewError};
use rusqlite::{Connection, OptionalExtension};
use std::path::Path as FsPath;
use transcript::{Act, Clip, ClipId, EditId, Instant, Name, Span, Text};

crate::statements! {
    LINE: Meaning =
        "SELECT id, clip_id, start_utc, end_utc, text, hidden_reason, edit_id, asr_model,
                speaker_label, word_timings, source_id
         FROM rendered_lines WHERE id = ?1";
    LINE_OF_EDIT: Meaning =
        "SELECT id FROM rendered_lines WHERE edit_id = ?1 AND hidden_reason IS NULL
         ORDER BY start_utc LIMIT 1";
    /// A session's shown lines between two, inclusive, in reading order.
    SESSION_LINES: Meaning =
        "SELECT id FROM rendered_lines
         WHERE source_id = ?1 AND hidden_reason IS NULL
           AND (start_utc, id) >= (?2, ?3) AND (start_utc, id) <= (?4, ?5)
         ORDER BY start_utc, id";
}

/// What the person asked for that cannot be done, or a store refusing.
#[derive(Debug)]
pub enum WriteError {
    /// Nothing but whitespace was typed.
    Blank,
    Missing(i64),
    /// The line is hidden: render left it out.
    Hidden(i64),
    /// A live line: no clip yet to hang an act on.
    Live(i64),
    /// Not hidden as "nobody spoke", so nothing to undo.
    NotNobodySpoke(i64),
    /// Not in a stretch a person excluded, so nothing to undo.
    NotExcluded(i64),
    /// No person's words that can still be taken back.
    NotCorrected(i64),
    /// No labelled fragment with this id.
    NotALabel(i64),
    /// An assignment whose lines are not one session's, in order.
    BadSpan,
    Db(rusqlite::Error),
    Log(LogError),
    View(ViewError),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(err) => write!(f, "{err}"),
            Self::Log(err) => write!(f, "{err}"),
            Self::View(err) => write!(f, "{err}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<rusqlite::Error> for WriteError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

impl From<LogError> for WriteError {
    fn from(err: LogError) -> Self {
        Self::Log(err)
    }
}

impl From<ViewError> for WriteError {
    fn from(err: ViewError) -> Self {
        Self::View(err)
    }
}

impl From<clips::ClipError> for WriteError {
    fn from(err: clips::ClipError) -> Self {
        Self::View(ViewError::Clip(err))
    }
}

/// A rendered line, shown or hidden, as the writes need it.
struct Line {
    id: i64,
    clip: Clip,
    span: Span,
    text: String,
    hidden: Option<String>,
    edit: Option<EditId>,
    by_person: bool,
    label: Option<String>,
    /// `(start_us, end_us, text)` per word, for a model line.
    words: Vec<(i64, i64, String)>,
}

fn line(meaning: &Connection, ingest: &Connection, id: i64) -> Result<Line, WriteError> {
    type Raw = (
        i64,
        Option<i64>,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let raw: Option<Raw> = LINE
        .query_row(meaning, [id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })
        .optional()?;
    let Some((id, clip, start, end, text, hidden, edit, model, label, words)) = raw else {
        return Err(WriteError::Missing(id));
    };
    let Some(clip) = clip else {
        return Err(WriteError::Live(id));
    };
    let clip = clips::by_id(ingest, ClipId::from_stored(clip))?.ok_or(WriteError::Missing(id))?;
    let span = Instant::parse(&start)
        .zip(Instant::parse(&end))
        .and_then(|(a, b)| Span::new(a, b))
        .ok_or(WriteError::Missing(id))?;
    let words = match words {
        Some(raw) => serde_json::from_str(&raw)
            .map_err(|err| WriteError::Db(rusqlite::Error::ToSqlConversionFailure(err.into())))?,
        None => Vec::new(),
    };
    Ok(Line {
        id,
        clip,
        span,
        text,
        hidden,
        edit: edit.map(EditId::from_stored),
        by_person: model.as_deref() == Some(crate::line::HUMAN_MODEL),
        label,
        words,
    })
}

fn shown(meaning: &Connection, ingest: &Connection, id: i64) -> Result<Line, WriteError> {
    let line = line(meaning, ingest, id)?;
    if line.hidden.is_some() {
        return Err(WriteError::Hidden(id));
    }
    Ok(line)
}

fn now() -> Instant {
    Instant::from_utc(chrono::Utc::now())
}

/// Append `acts` in one transaction, then render the clips they touch.
fn file(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    acts: &[Act],
    clips: &[ClipId],
) -> Result<Vec<EditId>, WriteError> {
    let tx = crate::sql::write_shared(meaning)?;
    let at = now();
    let ids = acts
        .iter()
        .map(|act| edit_log::append(&tx, at, act, None))
        .collect::<Result<Vec<_>, _>>()?;
    tx.commit()?;
    view::render_clips(meaning, ingest, root, clips)?;
    Ok(ids)
}

/// Exclude the stretch from line `first` to line `last` of one session, as if
/// recording had paused: only strangers spoke. The words are kept and not
/// filed as invented. One act per clip the stretch covers, filed together, so
/// one undo takes back all of it. Returns how many lines it hid.
///
/// # Errors
/// [`WriteError::BadSpan`] unless both lines are of one session, in order;
/// [`WriteError::Hidden`] for a hidden one.
pub fn exclude(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    first: i64,
    last: i64,
) -> Result<usize, WriteError> {
    let a = shown(meaning, ingest, first)?;
    let b = shown(meaning, ingest, last)?;
    if a.clip.source != b.clip.source || (b.span.start(), b.id) < (a.span.start(), a.id) {
        return Err(WriteError::BadSpan);
    }
    let lines = between(meaning, ingest, &a, &b)?;
    let mut per_clip: Vec<(ClipId, Instant, Instant)> = Vec::new();
    for l in &lines {
        match per_clip.last_mut() {
            Some((clip, _, stop)) if *clip == l.clip.id => *stop = (*stop).max(l.span.end()),
            _ => per_clip.push((l.clip.id, l.span.start(), l.span.end())),
        }
    }
    let acts: Vec<Act> = per_clip
        .iter()
        .filter_map(|(clip, from, to)| {
            Some(Act::Exclude {
                clip: *clip,
                span: Span::new(*from, *to)?,
            })
        })
        .collect();
    let clips: Vec<ClipId> = per_clip.iter().map(|(c, _, _)| *c).collect();
    file(meaning, ingest, root, &acts, &clips)?;
    Ok(lines.len())
}

/// The shown lines of `a`'s session from `a` to `b`, inclusive, in reading
/// order.
fn between(
    meaning: &Connection,
    ingest: &Connection,
    a: &Line,
    b: &Line,
) -> Result<Vec<Line>, WriteError> {
    let spelled = |l: &Line| audiocore::instant::python_isoformat_utc(l.span.start().to_utc());
    let ids: Vec<i64> = SESSION_LINES
        .prepare(meaning)?
        .query_map(
            rusqlite::params![a.clip.source.as_str(), spelled(a), a.id, spelled(b), b.id],
            |r| r.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    ids.into_iter()
        .map(|id| shown(meaning, ingest, id))
        .collect()
}

/// Take back an excluded stretch, by any of its hidden lines: every act filed
/// with that line's.
///
/// # Errors
/// [`WriteError::NotExcluded`] unless the line is hidden by a person's
/// exclusion.
pub fn undo_exclude(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
) -> Result<(), WriteError> {
    let line = line(meaning, ingest, id)?;
    let (Some(crate::line::EXCLUDED), Some(edit)) = (line.hidden.as_deref(), line.edit) else {
        return Err(WriteError::NotExcluded(id));
    };
    let mut acts = Vec::new();
    let mut clips = Vec::new();
    for e in edit_log::filed_with(meaning, edit)? {
        if let Act::Exclude { clip, .. } = e.act {
            acts.push(Act::Retract { edit: e.id });
            clips.push(clip);
        }
    }
    file(meaning, ingest, root, &acts, &clips)?;
    Ok(())
}

/// What a correction says besides the words.
#[derive(Debug, Default)]
pub struct Correction<'a> {
    pub speaker: Option<&'a str>,
    /// The person listened and vouches for the words, even if unchanged.
    pub words_checked: bool,
}

/// A person's words for a line, and who said them. Unchanged words file a
/// words act only when vouched for: renaming a speaker vouches for nothing.
/// Returns the line that shows the result.
///
/// # Errors
/// [`WriteError::Blank`], or the line is missing, hidden or live.
pub fn correct(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
    typed: &str,
    edit: &Correction,
) -> Result<i64, WriteError> {
    let text = Text::new(typed).ok_or(WriteError::Blank)?;
    let line = shown(meaning, ingest, id)?;
    let changed = text.as_str() != line.text;
    let mut acts = Vec::new();
    if changed || edit.words_checked {
        acts.push(Act::Words {
            clip: line.clip.id,
            span: line.span,
            over: line.span,
            text,
            checked: true,
        });
    }
    if let Some(name) = edit.speaker.and_then(Name::new)
        && Some(name.as_str()) != line.label.as_deref()
    {
        acts.push(Act::Speaker {
            clip: line.clip.id,
            span: line.span,
            name,
            enrol: true,
        });
    }
    if acts.is_empty() {
        return Ok(line.id);
    }
    let ids = file(meaning, ingest, root, &acts, &[line.clip.id])?;
    let words = ids.first().filter(|_| changed || edit.words_checked);
    Ok(match words {
        Some(edit) => LINE_OF_EDIT
            .query_row(meaning, [edit.stored()], |r| r.get(0))
            .optional()?
            .unwrap_or(line.id),
        None => line.id,
    })
}

/// Take back the person's words a line shows, for a mis-tap: the model's
/// show again.
///
/// # Errors
/// [`WriteError::NotCorrected`] for a line that is not a person's words.
pub fn undo_correction(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
) -> Result<(), WriteError> {
    let line = shown(meaning, ingest, id)?;
    let (true, Some(edit)) = (line.by_person, line.edit) else {
        return Err(WriteError::NotCorrected(id));
    };
    file(
        meaning,
        ingest,
        root,
        &[Act::Retract { edit }],
        &[line.clip.id],
    )?;
    Ok(())
}

/// A person listened and nobody spoke over the line.
///
/// # Errors
/// The line is missing, hidden or live.
pub fn no_speech(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
) -> Result<(), WriteError> {
    let line = shown(meaning, ingest, id)?;
    let act = Act::NoSpeech {
        clip: line.clip.id,
        span: line.span,
    };
    file(meaning, ingest, root, &[act], &[line.clip.id])?;
    Ok(())
}

/// Take back a "nobody spoke": the hidden line, by its id, shows again.
///
/// # Errors
/// [`WriteError::NotNobodySpoke`] unless the line is hidden that way.
pub fn undo_no_speech(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
) -> Result<(), WriteError> {
    let line = line(meaning, ingest, id)?;
    let nobody = crate::line::NOBODY_SPOKE.to_owned();
    let (Some(true), Some(edit)) = (line.hidden.as_deref().map(|h| h == nobody), line.edit) else {
        return Err(WriteError::NotNobodySpoke(id));
    };
    file(
        meaning,
        ingest,
        root,
        &[Act::Retract { edit }],
        &[line.clip.id],
    )?;
    Ok(())
}

/// A labelled fragment: a person's naming of a stretch, by its edit id.
fn label(meaning: &Connection, id: i64) -> Result<(ClipId, Span, Name), WriteError> {
    match edit_log::get(meaning, EditId::from_stored(id))? {
        Some(transcript::Edit {
            act: Act::Speaker {
                clip, span, name, ..
            },
            ..
        }) => Ok((clip, span, name)),
        _ => Err(WriteError::NotALabel(id)),
    }
}

/// Name a labelled fragment's voice again. Over the same span, the later act
/// wins.
///
/// # Errors
/// [`WriteError::NotALabel`] for an id that is not a person's naming.
pub fn relabel(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
    speaker: &str,
) -> Result<(), WriteError> {
    let (clip, span, _) = label(meaning, id)?;
    let name = Name::new(speaker).ok_or(WriteError::Blank)?;
    let act = Act::Speaker {
        clip,
        span,
        name,
        enrol: true,
    };
    file(meaning, ingest, root, &[act], &[clip])?;
    Ok(())
}

/// Keep a fragment's name but stop learning a voiceprint from it: a person
/// judged the clip unusable.
///
/// # Errors
/// [`WriteError::NotALabel`] for an id that is not a person's naming.
pub fn unenrol(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    id: i64,
) -> Result<(), WriteError> {
    let (clip, span, name) = label(meaning, id)?;
    let act = Act::Speaker {
        clip,
        span,
        name,
        enrol: false,
    };
    file(meaning, ingest, root, &[act], &[clip])?;
    Ok(())
}

/// Where a character offset of a line falls in time: at a word's edge for a
/// model line, in proportion for a person's.
fn instant_at(line: &Line, offset: usize, end: bool) -> Instant {
    if line.words.is_empty() {
        let chars = line.text.chars().count().max(1);
        let fraction = offset.min(chars) as f64 / chars as f64;
        return line
            .span
            .start()
            .plus_seconds(fraction * line.span.micros() as f64 / 1e6)
            .unwrap_or(line.span.start());
    }
    // The line's text is the words joined and trimmed.
    let lead = line.words.first().map_or(0, |(_, _, w)| {
        w.chars().count() - w.trim_start().chars().count()
    });
    let mut at: usize = 0;
    for (start, stop, word) in &line.words {
        let len = word.chars().count();
        let (from, to) = (at.saturating_sub(lead), (at + len).saturating_sub(lead));
        at += len;
        let inside = if end {
            offset > from && offset <= to
        } else {
            offset < to
        };
        if inside {
            let micros = if end { *stop } else { *start };
            return Instant::from_micros(micros).unwrap_or(line.span.start());
        }
    }
    if end {
        line.span.end()
    } else {
        line.span.start()
    }
}

/// Give a text span, from a character of one line to a character of another
/// in the same session, to `speaker`: one naming act per clip it touches.
/// Returns how many lines it touched.
///
/// # Errors
/// [`WriteError::BadSpan`] for lines of another session or out of order.
pub fn assign_span(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    source: &str,
    (first, first_char): (i64, usize),
    (last, last_char): (i64, usize),
    speaker: &str,
) -> Result<usize, WriteError> {
    let name = Name::new(speaker).ok_or(WriteError::Blank)?;
    let a = shown(meaning, ingest, first)?;
    let b = shown(meaning, ingest, last)?;
    if a.clip.source.as_str() != source || b.clip.source.as_str() != source || b.span < a.span {
        return Err(WriteError::BadSpan);
    }
    let start = instant_at(&a, first_char, false);
    let end = instant_at(&b, last_char, true);
    let lines = between(meaning, ingest, &a, &b)?;
    let mut per_clip: Vec<(ClipId, Instant, Instant)> = Vec::new();
    for l in &lines {
        let from = if l.id == a.id { start } else { l.span.start() };
        let to = if l.id == b.id { end } else { l.span.end() };
        match per_clip.last_mut() {
            Some((clip, _, stop)) if *clip == l.clip.id => *stop = to,
            _ => per_clip.push((l.clip.id, from, to)),
        }
    }
    let acts: Vec<Act> = per_clip
        .iter()
        .filter_map(|(clip, from, to)| {
            Some(Act::Speaker {
                clip: *clip,
                span: Span::new(*from, *to)?,
                name: name.clone(),
                enrol: true,
            })
        })
        .collect();
    let clips: Vec<ClipId> = per_clip.iter().map(|(c, _, _)| *c).collect();
    file(meaning, ingest, root, &acts, &clips)?;
    Ok(lines.len())
}

/// The session acts of `source` still in force.
fn in_force(meaning: &Connection, source: &str) -> Result<Vec<transcript::Edit>, WriteError> {
    let edits = edit_log::for_source(meaning, source)?;
    let gone = transcript::taken_back(&edits);
    Ok(edits
        .into_iter()
        .filter(|e| !gone.contains(&e.id))
        .collect())
}

fn session_clips(ingest: &Connection, source: &str) -> Result<Vec<ClipId>, WriteError> {
    Ok(clips::of_source(ingest, source)?
        .into_iter()
        .map(|c| c.id)
        .collect())
}

/// Name a diarized voice across a session, or with `None` take the name back.
/// Returns whether anything changed.
///
/// # Errors
/// If a store refuses.
pub fn name_voice(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    source: &transcript::SourceId,
    cluster: &str,
    name: Option<&str>,
) -> Result<bool, WriteError> {
    let current: Vec<EditId> = in_force(meaning, source.as_str())?
        .into_iter()
        .filter(|e| matches!(&e.act, Act::Voice { cluster: c, .. } if c == cluster))
        .map(|e| e.id)
        .collect();
    let acts: Vec<Act> = match name.and_then(Name::new) {
        Some(name) => vec![Act::Voice {
            source: source.clone(),
            cluster: cluster.to_owned(),
            name,
        }],
        None => current
            .into_iter()
            .map(|edit| Act::Retract { edit })
            .collect(),
    };
    if acts.is_empty() {
        return Ok(false);
    }
    file(
        meaning,
        ingest,
        root,
        &acts,
        &session_clips(ingest, source.as_str())?,
    )?;
    Ok(true)
}

/// Pin `source`'s language, or with `None` leave it to the model.
///
/// # Errors
/// If a store refuses.
pub fn pin_language(
    meaning: &Connection,
    ingest: &Connection,
    root: &FsPath,
    source: &transcript::SourceId,
    language: Option<transcript::Language>,
) -> Result<(), WriteError> {
    let current: Vec<EditId> = in_force(meaning, source.as_str())?
        .into_iter()
        .filter(|e| matches!(e.act, Act::Language { .. }))
        .map(|e| e.id)
        .collect();
    let acts: Vec<Act> = match language {
        Some(language) => vec![Act::Language {
            source: source.clone(),
            language,
        }],
        None => current
            .into_iter()
            .map(|edit| Act::Retract { edit })
            .collect(),
    };
    if acts.is_empty() {
        return Ok(());
    }
    file(
        meaning,
        ingest,
        root,
        &acts,
        &session_clips(ingest, source.as_str())?,
    )?;
    Ok(())
}

/// The language a person pinned `source` to, if any: what its transcriptions
/// are asked for.
///
/// # Errors
/// If a store refuses.
pub fn language_of(
    meaning: &Connection,
    source: &str,
) -> Result<Option<transcript::Language>, WriteError> {
    Ok(in_force(meaning, source)?
        .into_iter()
        .rev()
        .find_map(|e| match e.act {
            Act::Language { language, .. } => Some(language),
            _ => None,
        }))
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, work};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Run a write against both planes off the async runtime.
#[expect(clippy::result_large_err, reason = "the Err is the HTTP response")]
async fn write<T: Send + 'static>(
    st: &reads::State,
    what: &'static str,
    run: impl FnOnce(&Connection, &Connection, &FsPath) -> Result<T, WriteError> + Send + 'static,
) -> Result<T, Response> {
    let root = st.root.clone();
    match tokio::task::spawn_blocking(move || {
        let meaning = work::open_write(&root).map_err(WriteError::Db)?;
        let ingest = crate::store::open(&root).map_err(WriteError::Db)?;
        run(&meaning, &ingest, &root)
    })
    .await
    {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(refused(what, err)),
        Err(err) => Err(route::faulted(what, &err)),
    }
}

#[derive(Deserialize)]
pub struct ReassignIn {
    speaker: String,
}

pub async fn correction_reassign_route(
    State(st): State<Arc<reads::State>>,
    Path(id): Path<i64>,
    Json(body): Json<ReassignIn>,
) -> Response {
    let speaker = body.speaker.trim().to_owned();
    if speaker.is_empty() {
        return (StatusCode::BAD_REQUEST, "speaker required").into_response();
    }
    match write(&st, "correction reassign", move |m, i, root| {
        relabel(m, i, root, id, &speaker)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn correction_hide_route(
    State(st): State<Arc<reads::State>>,
    Path(id): Path<i64>,
) -> Response {
    match write(&st, "correction hide", move |m, i, root| {
        unenrol(m, i, root, id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "CorrectRequest")]
pub struct CorrectIn {
    id: i64,
    text: String,
    #[ts(optional = nullable)]
    speaker: Option<String>,
    #[ts(optional = nullable)]
    checked: Option<bool>,
}

pub async fn correct_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<CorrectIn>,
) -> Response {
    match write(&st, "correct", move |m, i, root| {
        correct(
            m,
            i,
            root,
            body.id,
            &body.text,
            &Correction {
                speaker: body.speaker.as_deref(),
                words_checked: body.checked == Some(true),
            },
        )
    })
    .await
    {
        Ok(new_id) => Json(route::NewId { new_id }).into_response(),
        Err(response) => response,
    }
}

/// One line, by id: nobody spoke, and the undos.
#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "LineRequest")]
pub struct LineIn {
    id: i64,
}

pub async fn no_speech_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<LineIn>,
) -> Response {
    match write(&st, "no speech", move |m, i, root| {
        no_speech(m, i, root, body.id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn undo_no_speech_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<LineIn>,
) -> Response {
    match write(&st, "undo no speech", move |m, i, root| {
        undo_no_speech(m, i, root, body.id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

/// A stretch, by its first and last line.
#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "ExcludeRequest")]
pub struct ExcludeIn {
    from: i64,
    to: i64,
}

pub async fn exclude_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<ExcludeIn>,
) -> Response {
    match write(&st, "exclude", move |m, i, root| {
        exclude(m, i, root, body.from, body.to)
    })
    .await
    {
        Ok(_) => route::ack(),
        Err(response) => response,
    }
}

pub async fn undo_exclude_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<LineIn>,
) -> Response {
    match write(&st, "undo exclude", move |m, i, root| {
        undo_exclude(m, i, root, body.id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn undo_correction_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<LineIn>,
) -> Response {
    match write(&st, "undo correction", move |m, i, root| {
        undo_correction(m, i, root, body.id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

/// A text span, from a character of one line to a character of another,
/// given to a speaker. Offsets count characters (the app counts UTF-16 units,
/// the same for BMP text).
#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "AssignSpanRequest")]
#[serde(rename_all = "camelCase")]
pub struct AssignIn {
    start_turn: i64,
    start_char: usize,
    end_turn: i64,
    end_char: usize,
    name: String,
}

#[derive(Serialize, ts_rs::TS)]
#[ts(export, rename = "AssignResult")]
pub struct AssignOut {
    touched: i64,
}

pub async fn assign_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
    Json(body): Json<AssignIn>,
) -> Response {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name required").into_response();
    }
    let root = st.root.clone();
    let assigned = tokio::task::spawn_blocking(move || {
        let meaning = work::open_write(&root).map_err(WriteError::Db)?;
        let ingest = crate::store::open(&root).map_err(WriteError::Db)?;
        assign_span(
            &meaning,
            &ingest,
            &root,
            &source,
            (body.start_turn, body.start_char),
            (body.end_turn, body.end_char),
            &name,
        )
    });
    match assigned.await {
        Ok(Ok(touched)) => Json(AssignOut {
            touched: i64::try_from(touched).unwrap_or(i64::MAX),
        })
        .into_response(),
        Ok(Err(err)) => refused("assign span", err),
        Err(err) => route::faulted("assign span task", &err),
    }
}

/// A refused write, as the caller's to fix (each says which) or a fault.
pub fn refused(what: &str, err: WriteError) -> Response {
    let bad = |message: String| (StatusCode::BAD_REQUEST, message).into_response();
    match err {
        WriteError::Blank => bad("text must not be blank".to_owned()),
        WriteError::Missing(id) => bad(format!("no line with id {id}")),
        WriteError::Hidden(id) => bad(format!("line #{id} is hidden")),
        WriteError::Live(id) => bad(format!(
            "line #{id} is a live guess: wait for its transcription"
        )),
        WriteError::NotNobodySpoke(id) => bad(format!("line #{id} is not hidden as nobody spoke")),
        WriteError::NotExcluded(id) => bad(format!("line #{id} is not in an excluded stretch")),
        WriteError::NotCorrected(id) => bad(format!(
            "line #{id} is not a person's words that can be taken back"
        )),
        WriteError::NotALabel(id) => bad(format!("no labelled fragment with id {id}")),
        WriteError::BadSpan => bad("the span's lines are not one session's, in order".to_owned()),
        WriteError::Db(err) => route::faulted(what, &err),
        WriteError::Log(err) => route::faulted(what, &err),
        WriteError::View(err) => route::faulted(what, &err),
    }
}
