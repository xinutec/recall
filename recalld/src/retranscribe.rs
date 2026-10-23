//! Transcribing a clip again. A clip keeps only its current result, so a
//! request is the job itself, queued again: it keeps showing its result until
//! the new one replaces it, and a request still waiting can be taken back.

use audiocore::job::Kind;
use rusqlite::Connection;

crate::statements! {
    REQUEUE: Ingest =
        "UPDATE jobs SET state = 'queued', leased_until = NULL, done_utc = NULL,
                             attempts = 0
             WHERE kind = ?1 AND filename = ?2 AND done_utc IS NOT NULL";
    /// A re-run taken back while waiting: done again, with the result it kept.
    KEEP_RESULT: Ingest =
        "UPDATE jobs SET state = 'done', done_utc = ?2, leased_until = NULL
         WHERE kind = ?3 AND filename = ?1 AND done_utc IS NULL AND result IS NOT NULL";
    CANDIDATES: Ingest =
        "SELECT j.filename, j.result FROM jobs j
         WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL";
}

/// What [`request`] did with each name it was given.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Requested {
    /// Back in the queue.
    pub queued: Vec<String>,
    /// No finished transcription to redo; left as they were.
    pub skipped: Vec<String>,
}

/// Queue `filenames` to be transcribed again.
///
/// # Errors
/// If the ingest plane refuses; each clip is one transaction.
pub fn request(ingest: &Connection, filenames: &[String]) -> rusqlite::Result<Requested> {
    let mut out = Requested::default();
    for filename in filenames {
        let tx = crate::sql::write_shared(ingest)?;
        let requeued = REQUEUE.execute(&tx, (Kind::TranscribeSegment, filename))?;
        if requeued == 0 {
            out.skipped.push(filename.clone());
            continue;
        }
        tx.commit()?;
        out.queued.push(filename.clone());
    }
    Ok(out)
}

/// A clip whose stored transcription lost speech to a repetition loop.
#[derive(Debug, PartialEq, serde::Serialize)]
pub struct Candidate {
    pub filename: String,
    /// Seconds of measured speech under segments that read as repetition loops
    /// (`quality::is_repetition_loop`).
    pub looped_speech_s: f64,
}

/// Every clip not already waiting with more than `min_speech_s` of measured
/// speech under looping segments, most lost first.
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
        let Some(regions) = crate::speech::evidence(ingest, &filename)?.regions else {
            continue;
        };
        let looped_speech_s: f64 = looped
            .iter()
            .flat_map(|&(a, b)| {
                regions
                    .iter()
                    .map(move |&(start, end)| (b.min(end) - a.max(start)).max(0.0))
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

/// What a take-back did.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Undone {
    /// The request was still waiting: the job is done again with the result
    /// it kept all along.
    Cancelled,
}

/// Take back a request still waiting, `true` if there was one: the job is
/// done again with the result it kept.
///
/// # Errors
/// If the ingest plane refuses.
pub fn cancel(
    ingest: &Connection,
    filename: &str,
    now: &audiocore::instant::Stamp,
) -> rusqlite::Result<bool> {
    Ok(KEEP_RESULT.execute(ingest, (filename, now.to_string(), Kind::TranscribeSegment))? > 0)
}

// --- the HTTP surface --------------------------------------------------------

use crate::{reads, route};
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

pub async fn request_route(
    State(st): State<Arc<reads::State>>,
    asked: crate::webauth::Asked,
    Json(body): Json<RequestIn>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    if body.filenames.len() > MAX_PER_REQUEST {
        return (
            StatusCode::BAD_REQUEST,
            format!("at most {MAX_PER_REQUEST} clips per request"),
        )
            .into_response();
    }
    let root = st.root.clone();
    route::json("retranscribe", move || {
        // Someone else's clip is answered as one that does not exist: skipped.
        let (mine, theirs) = own_clips(&root, viewer.as_ref(), body.filenames)?;
        let mut out = request(&crate::store::open(&root)?, &mine)?;
        out.skipped.extend(theirs);
        Ok(out)
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

/// The asker's own clips only.
pub async fn candidates_route(
    State(st): State<Arc<reads::State>>,
    asked: crate::webauth::Asked,
    axum::extract::Query(q): axum::extract::Query<CandidatesQuery>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    route::json("retranscribe candidates", move || {
        let ingest = crate::store::open(&root)?;
        let mut found = candidates(&ingest, q.min_speech_s)?;
        let seen = reads::open_as(&root, viewer.as_ref())?;
        if let Some(mine) = reads::own_sources(&seen, viewer.as_ref())? {
            let mut kept = Vec::new();
            for candidate in found {
                let clip = crate::clips::by_filename(&ingest, &candidate.filename)
                    .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
                if clip.is_some_and(|c| mine.contains(c.source.as_str())) {
                    kept.push(candidate);
                }
            }
            found = kept;
        }
        Ok(found)
    })
    .await
}

/// `names` split into the viewer's own clips and the rest; a request no session
/// carried owns them all.
fn own_clips(
    root: &std::path::Path,
    viewer: Option<&crate::webauth::Viewer>,
    names: Vec<String>,
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    if viewer.is_none() {
        return Ok((names, Vec::new()));
    }
    let seen = reads::open_as(root, viewer)?;
    let (mut mine, mut theirs) = (Vec::new(), Vec::new());
    for name in names {
        if reads::sees(&seen, root, &[reads::Target::Clip(name.clone())])? {
            mine.push(name);
        } else {
            theirs.push(name);
        }
    }
    Ok((mine, theirs))
}

#[derive(Deserialize)]
pub struct UndoIn {
    filename: String,
}

/// A request still waiting is cancelled. A finished one is gone for good: a
/// clip keeps only its current result, so there is no older one to restore.
pub async fn undo_route(
    State(st): State<Arc<reads::State>>,
    asked: crate::webauth::Asked,
    Json(body): Json<UndoIn>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    let cancelled = tokio::task::spawn_blocking(move || {
        // Someone else's clip has nothing waiting, as far as the asker knows.
        let (mine, _) = own_clips(&root, viewer.as_ref(), vec![body.filename])?;
        let Some(filename) = mine.first() else {
            return Ok(false);
        };
        cancel(
            &crate::store::open(&root)?,
            filename,
            &audiocore::instant::Stamp::now(),
        )
    });
    match cancelled.await {
        Ok(Ok(true)) => Json(Undone::Cancelled).into_response(),
        Ok(Ok(false)) => (
            StatusCode::GONE,
            "a re-transcription replaces the clip's result; there is no older one to restore",
        )
            .into_response(),
        Ok(Err(err)) => route::faulted("retranscribe undo", &err),
        Err(err) => route::faulted("retranscribe undo task", &err),
    }
}
