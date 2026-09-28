//! Transcribing a clip again, without losing what a person did to it.
//!
//! The turns pass writes a clip once: a clip with lines is skipped. A clip whose
//! lines are wrong (a Whisper loop, #1764) therefore needs a request, which
//! does three things in order:
//!
//! 1. [`request`] puts the clip's transcription job back in the queue and
//!    records the request. The speaker pass leaves a requested clip alone.
//! 2. When the new words arrive, the turns pass sets the clip's machine lines
//!    aside ([`HiddenReason::SetAside`]) and writes the new ones around every
//!    span a person owns, in one transaction.
//! 3. [`written`] then drops the request and the speaker pass's ledger row, so
//!    speakers are added to the NEW lines. Releasing it earlier lets the speaker
//!    pass write its own lines onto an empty clip first, which the turns pass
//!    then skips.
//!
//! Nothing is deleted: [`undo`] hides the new lines and shows the old ones.

use crate::turn_store::{self, HiddenReason};
use audiocore::job::Kind;
use rusqlite::{Connection, OptionalExtension};

crate::statements! {
    REQUEUE: Ingest =
        "UPDATE jobs SET state = 'queued', leased_until = NULL, done_utc = NULL,
                             result = NULL, attempts = 0
             WHERE kind = ?1 AND filename = ?2 AND done_utc IS NOT NULL";
    REQUEST: Ingest =
        "INSERT OR REPLACE INTO retranscribe_requests (filename, requested_utc)
             VALUES (?1, ?2)";
    CANDIDATES: Ingest =
        "SELECT j.filename, j.result FROM jobs j
         WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM retranscribe_requests r WHERE r.filename = j.filename)";
    IS_REQUESTED: Ingest =
        "SELECT 1 FROM retranscribe_requests WHERE filename = ?1";
    CLEAR_LEDGER: Ingest =
        "DELETE FROM pass_ledger WHERE kind = ?1 AND filename = ?2";
    DROP_REQUEST: Ingest =
        "DELETE FROM retranscribe_requests WHERE filename = ?1";
    SEGMENT: Ingest =
        "SELECT source, start_utc FROM segments WHERE filename = ?1";
    AUDIO_SEGMENT: Meaning =
        "SELECT id FROM audio_segments WHERE source_id = ?1 AND start_utc = ?2";
    SET_ASIDE: Meaning =
        "SELECT count(*) FROM transcript_segments
         WHERE audio_segment_id = ?1 AND hidden_reason = ?2 AND superseded_by IS NULL";
}

/// What [`request`] did with each name it was given.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Requested {
    /// Back in the queue.
    pub queued: Vec<String>,
    /// No finished transcription to redo: never transcribed, still in the
    /// queue, or not a clip at all. Left as they were.
    pub skipped: Vec<String>,
}

/// Queue `filenames` to be transcribed again.
///
/// # Errors
/// If the ingest plane refuses; nothing is half-requested, each clip is one
/// transaction.
pub fn request(
    ingest: &Connection,
    filenames: &[String],
    now: &audiocore::instant::Stamp,
) -> rusqlite::Result<Requested> {
    let mut out = Requested::default();
    for filename in filenames {
        let tx = crate::sql::write_shared(ingest)?;
        let requeued = REQUEUE.execute(&tx, (Kind::TranscribeSegment, filename))?;
        if requeued == 0 {
            out.skipped.push(filename.clone());
            continue;
        }
        CLEAR_LEDGER.execute(&tx, (Kind::TranscribeSegment, filename))?;
        REQUEST.execute(&tx, (filename, now.to_string()))?;
        tx.commit()?;
        out.queued.push(filename.clone());
    }
    Ok(out)
}

/// A clip whose stored transcription lost speech to a repetition loop.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Candidate {
    pub filename: String,
    /// Seconds of measured speech under the segments the passes drop as loops.
    pub looped_speech_s: f64,
}

/// Every clip with more than `min_speech_s` of measured speech under segments
/// the passes drop as loops (`quality::is_repetition_loop`), most lost first.
/// Clips already waiting are left out. Reads the stored results only: the
/// speech is what the speech pass placed, not what the model claimed.
///
/// # Errors
/// If the ingest plane refuses.
pub fn candidates(ingest: &Connection, min_speech_s: f64) -> rusqlite::Result<Vec<Candidate>> {
    let mut stmt = CANDIDATES.prepare(ingest)?;
    let rows = stmt.query_map([Kind::TranscribeSegment], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (filename, raw) = row?;
        let Some(heard) = audiocore::shim::Stored::<audiocore::shim::asr::Reply>::parse(&raw)
            .ok()
            .and_then(audiocore::shim::Stored::answer)
        else {
            continue;
        };
        let looped: Vec<(f64, f64)> = heard
            .segments
            .iter()
            .filter(|s| crate::quality::is_repetition_loop(&s.text))
            .filter_map(|s| Some((s.start?, s.end?)))
            .collect();
        if looped.is_empty() {
            continue;
        }
        let Some(regions) = crate::speech::heard(ingest, &filename)?.regions else {
            continue;
        };
        let looped_speech_s: f64 = looped
            .iter()
            .flat_map(|&(a, b)| {
                regions
                    .iter()
                    .map(move |r| (b.min(r.end) - a.max(r.start)).max(0.0))
            })
            .sum();
        if looped_speech_s > min_speech_s {
            out.push(Candidate {
                filename,
                looped_speech_s,
            });
        }
    }
    out.sort_by(|a, b| b.looped_speech_s.total_cmp(&a.looped_speech_s));
    Ok(out)
}

/// Whether `filename` is waiting to be transcribed again.
///
/// # Errors
/// If the ingest plane refuses.
pub fn is_requested(ingest: &Connection, filename: &str) -> rusqlite::Result<bool> {
    Ok(IS_REQUESTED
        .query_row(ingest, [filename], |r| r.get::<_, i64>(0))
        .optional()?
        .is_some())
}

/// The turns pass has written the new lines (or found nothing to write): drop
/// the request and let the speaker pass decide the clip again.
///
/// # Errors
/// If the ingest plane refuses.
pub fn written(ingest: &Connection, filename: &str) -> rusqlite::Result<()> {
    let tx = crate::sql::write_shared(ingest)?;
    DROP_REQUEST.execute(&tx, [filename])?;
    CLEAR_LEDGER.execute(&tx, (Kind::DiarizeSegment, filename))?;
    tx.commit()
}

/// Why a re-transcription could not be taken back.
#[derive(Debug)]
pub enum UndoError {
    /// No clip by that name, or its audio is not registered.
    Missing,
    /// Nothing of this clip was set aside: it was never transcribed again, or
    /// it was already taken back.
    NothingSetAside,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for UndoError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// What [`undo`] did.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Undone {
    /// The request was still waiting: dropped, and the old lines never moved.
    /// A new transcription that lands later is not written.
    Cancelled,
    /// The new lines gave way to the ones set aside.
    Restored,
}

/// Take a re-transcription back: the machine lines written since give way and
/// the ones set aside show again. A person's lines are untouched either way.
///
/// # Errors
/// [`UndoError::Missing`] or [`UndoError::NothingSetAside`]; on database
/// failure, nothing changes.
pub fn undo(
    meaning: &mut Connection,
    ingest: &Connection,
    filename: &str,
) -> Result<Undone, UndoError> {
    let cancelled = DROP_REQUEST.execute(ingest, [filename])?;
    if cancelled > 0 {
        return Ok(Undone::Cancelled);
    }
    let (source, start): (String, String) = SEGMENT
        .query_row(ingest, [filename], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?
        .ok_or(UndoError::Missing)?;
    let block_start = chrono::DateTime::parse_from_rfc3339(&start)
        .map_err(|_| UndoError::Missing)?
        .with_timezone(&chrono::Utc);
    let audio_id: i64 = AUDIO_SEGMENT
        .query_row(
            meaning,
            rusqlite::params![
                source,
                audiocore::instant::python_isoformat_utc(block_start)
            ],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(UndoError::Missing)?;
    let tx = crate::sql::write(meaning)?;
    let set_aside: i64 = SET_ASIDE.query_row(
        &tx,
        rusqlite::params![audio_id, HiddenReason::SetAside.to_string()],
        |r| r.get(0),
    )?;
    if set_aside == 0 {
        return Err(UndoError::NothingSetAside);
    }
    turn_store::hide_machine_turns(&tx, audio_id, &HiddenReason::RetranscriptionUndone)?;
    turn_store::unhide_all(&tx, audio_id, &HiddenReason::SetAside)?;
    tx.commit()?;
    Ok(Undone::Restored)
}

// --- the HTTP surface --------------------------------------------------------

use crate::{reads, route, work};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

/// At most this many clips per request: a batch, not the archive at once.
pub const MAX_PER_REQUEST: usize = 1000;

#[derive(Deserialize)]
pub struct RequestIn {
    filenames: Vec<String>,
}

#[derive(Deserialize)]
pub struct UndoIn {
    filename: String,
}

pub async fn request_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<RequestIn>,
) -> Response {
    if body.filenames.len() > MAX_PER_REQUEST {
        return (
            StatusCode::BAD_REQUEST,
            format!("at most {MAX_PER_REQUEST} clips per request"),
        )
            .into_response();
    }
    let root = st.root.clone();
    let now = audiocore::instant::Stamp::now();
    route::json("retranscribe", move || {
        request(&crate::store::open(&root)?, &body.filenames, &now)
    })
    .await
}

#[derive(Deserialize)]
pub struct CandidatesQuery {
    /// Seconds of speech a clip must have lost; 1 by default.
    #[serde(default = "one_second")]
    min_speech_s: f64,
}

const fn one_second() -> f64 {
    1.0
}

pub async fn candidates_route(
    State(st): State<Arc<reads::State>>,
    axum::extract::Query(q): axum::extract::Query<CandidatesQuery>,
) -> Response {
    let root = st.root.clone();
    route::json("retranscribe candidates", move || {
        candidates(&crate::store::open(&root)?, q.min_speech_s)
    })
    .await
}

pub async fn undo_route(State(st): State<Arc<reads::State>>, Json(body): Json<UndoIn>) -> Response {
    let root = st.root.clone();
    let undone = tokio::task::spawn_blocking(move || {
        let mut meaning = work::open_write(&root)?;
        let ingest = crate::store::open(&root)?;
        undo(&mut meaning, &ingest, &body.filename)
    });
    match undone.await {
        Ok(Ok(done)) => Json(done).into_response(),
        Ok(Err(UndoError::Missing)) => (StatusCode::NOT_FOUND, "no such clip").into_response(),
        Ok(Err(UndoError::NothingSetAside)) => (
            StatusCode::BAD_REQUEST,
            "nothing of this clip was set aside: never transcribed again, or already taken back",
        )
            .into_response(),
        Ok(Err(UndoError::Db(err))) => route::faulted("retranscribe undo", &err),
        Err(err) => route::faulted("retranscribe undo task", &err),
    }
}
