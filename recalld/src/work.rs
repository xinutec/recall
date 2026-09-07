//! Vocabulary and on-demand refine (stage F1), ported from `recall.api_work`.
//!
//! These are the first routes in recalld that WRITE `recall.sqlite`, so read the
//! ownership note before adding more.
//!
//! ⚠ **Why recalld may now write a plane it does not own.** [`crate::reads`] opens
//! this database READ-ONLY on purpose: while the Python API is the only writer,
//! recalld must not be *able* to write it. That rule holds for everything derived
//! — turns, tiers, attribution — and it is not being relaxed for those. What
//! changes here is narrower: F1 moves the browsing tier route group by route
//! group, and a route group that writes cannot move at all under a read-only
//! connection. So writes are opened per route, on their own connection, with the
//! same WAL + busy-timeout discipline the Mac's agents already use on their copy.
//! Two writers on one `SQLite` file is the documented model
//! (docs/architecture.md, "recalld — the Isis daemon"), not an exception being
//! carved here.
//!
//! ⚠ **Both of these are core, despite where they used to live.** Vocabulary is
//! the household's proper nouns, fed to Whisper as `initial_prompt` on every pass
//! — the cheap lever for requirement #2, and the Rust runner refuses to
//! transcribe without it. Refine queues a diarized re-derivation of one stretch
//! and runs no ML inline; the idle-gated daemon executes it so the heavy pass
//! stays off live capture.

use crate::reads;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Open `recall.sqlite` for writing.
///
/// ⚠ Separate from [`reads::open`] deliberately, and not a replacement for it: a
/// read route must keep taking the read-only handle, so that a bug in a read path
/// cannot write. The busy timeout is what makes two writers civil — the Python
/// tier holds this file too, and a writer that failed instead of waiting would
/// turn ordinary contention into a 500.
pub fn open_write(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(root.join("recall.sqlite"))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Term {
    pub id: i64,
    pub term: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct VocabularyOut {
    pub items: Vec<Term>,
}

/// The terms, ordered case-insensitively — the order the Labels page renders.
pub fn vocabulary(conn: &Connection) -> rusqlite::Result<VocabularyOut> {
    let mut stmt = conn.prepare("SELECT id, term FROM vocabulary ORDER BY term COLLATE NOCASE")?;
    let rows = stmt.query_map([], |r| {
        Ok(Term {
            id: r.get(0)?,
            term: r.get(1)?,
        })
    })?;
    Ok(VocabularyOut {
        items: rows.collect::<rusqlite::Result<Vec<_>>>()?,
    })
}

/// Why a term was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum TermError {
    Blank,
}

/// Add a term, returning the id — the existing one if it is already there.
///
/// ⚠ Idempotent by design: `ON CONFLICT DO NOTHING` then read the id back, so
/// adding a term twice is not an error and does not create a duplicate. The
/// Labels page has no way to know what is already in the list before it posts.
pub fn add_term(conn: &Connection, term: &str, now: &str) -> Result<i64, TermError> {
    let cleaned = term.trim();
    if cleaned.is_empty() {
        // A blank term would be applied to every transcription as an empty prompt
        // fragment and could never be found again to delete.
        return Err(TermError::Blank);
    }
    conn.execute(
        "INSERT INTO vocabulary (term, created_utc) VALUES (?1, ?2) ON CONFLICT(term) DO NOTHING",
        (cleaned, now),
    )
    .map_err(|_| TermError::Blank)?;
    conn.query_row(
        "SELECT id FROM vocabulary WHERE term = ?1",
        [cleaned],
        |r| r.get(0),
    )
    .map_err(|_| TermError::Blank)
}

pub fn delete_term(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM vocabulary WHERE id = ?1", [id])?;
    Ok(())
}

/// Queue a diarized re-derivation of `[start, end)` of one recording.
pub fn add_refine_request(
    conn: &Connection,
    source: &str,
    start: &str,
    end: &str,
    now: &str,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO refine_requests (source_id, start_utc, end_utc, created_utc) \
         VALUES (?1, ?2, ?3, ?4)",
        (source, start, end, now),
    )?;
    Ok(conn.last_insert_rowid())
}

// --- HTTP ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TermIn {
    term: String,
}

#[derive(Deserialize)]
pub struct RefineIn {
    source: String,
    start: String,
    end: String,
}

#[derive(Serialize)]
struct NewId {
    #[serde(rename = "newId")]
    new_id: i64,
}

#[derive(Serialize)]
struct Ok_ {
    ok: bool,
}

fn ok() -> Response {
    Json(Ok_ { ok: true }).into_response()
}

fn oops(context: &str, err: &rusqlite::Error) -> Response {
    tracing::warn!("{context} failed: {err}");
    (StatusCode::INTERNAL_SERVER_ERROR, "write failed").into_response()
}

/// An ISO-8601 instant, or a 400. Never a substituted "now".
///
/// ⚠ Manufacturing a time for a malformed one would queue a refine over the wrong
/// stretch of audio — silently, since the request would succeed.
fn instant(value: &str) -> Option<String> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc).to_rfc3339())
}

pub async fn vocabulary_route(
    axum::extract::State(st): axum::extract::State<Arc<reads::State>>,
) -> Response {
    let root = st.root.clone();
    match tokio::task::spawn_blocking(move || vocabulary(&reads::open(&root)?)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => oops("vocabulary read", &e),
        Err(e) => {
            tracing::warn!("vocabulary task failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response()
        }
    }
}

pub async fn vocabulary_add_route(
    axum::extract::State(st): axum::extract::State<Arc<reads::State>>,
    Json(body): Json<TermIn>,
) -> Response {
    let root = st.root.clone();
    let now = chrono::Utc::now().to_rfc3339();
    match tokio::task::spawn_blocking(move || {
        let conn = open_write(&root).map_err(|e| format!("open: {e}"))?;
        add_term(&conn, &body.term, &now).map_err(|_| "blank".to_string())
    })
    .await
    {
        Ok(Ok(id)) => Json(NewId { new_id: id }).into_response(),
        Ok(Err(why)) if why == "blank" => {
            (StatusCode::BAD_REQUEST, "vocabulary term must not be blank").into_response()
        }
        Ok(Err(why)) => {
            tracing::warn!("vocabulary add failed: {why}");
            (StatusCode::INTERNAL_SERVER_ERROR, "write failed").into_response()
        }
        Err(e) => {
            tracing::warn!("vocabulary add task failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "write failed").into_response()
        }
    }
}

pub async fn vocabulary_delete_route(
    axum::extract::State(st): axum::extract::State<Arc<reads::State>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let root = st.root.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = open_write(&root)?;
        delete_term(&conn, id)
    })
    .await
    {
        Ok(Ok(())) => ok(),
        Ok(Err(e)) => oops("vocabulary delete", &e),
        Err(e) => {
            tracing::warn!("vocabulary delete task failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "write failed").into_response()
        }
    }
}

pub async fn refine_route(
    axum::extract::State(st): axum::extract::State<Arc<reads::State>>,
    Json(body): Json<RefineIn>,
) -> Response {
    let (Some(start), Some(end)) = (instant(&body.start), instant(&body.end)) else {
        return (StatusCode::BAD_REQUEST, "start and end must be ISO-8601").into_response();
    };
    let root = st.root.clone();
    let now = chrono::Utc::now().to_rfc3339();
    match tokio::task::spawn_blocking(move || {
        let conn = open_write(&root)?;
        add_refine_request(&conn, &body.source, &start, &end, &now)
    })
    .await
    {
        Ok(Ok(_)) => ok(),
        Ok(Err(e)) => oops("refine enqueue", &e),
        Err(e) => {
            tracing::warn!("refine task failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "write failed").into_response()
        }
    }
}
