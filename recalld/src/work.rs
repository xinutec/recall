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

use crate::{reads, route};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension};
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
///
/// ⚠ **30 s because that is what the OTHER writer waits, and the shorter side
/// decides.** This said 5 s until 2026-09-08, and the paragraph above described
/// exactly the failure that produced: within ten minutes of `/sync/capture`
/// cutting over, one mirror handshake in 116 came back 500 with `database is
/// locked`. The Python's `Store` sets `PRAGMA busy_timeout = 30000` and had
/// answered 104,482 of these without a single 500, so giving up six times sooner
/// was the whole of the difference. `audiod`'s speech scanner already waits 30 s
/// on this same file for the same reason.
///
/// ⚠ `GET /api/capture` takes this handle too, so a phone's poll now waits rather
/// than 500s under contention. That is the right way round for a control: a late
/// correct answer beats a prompt failure, and contention here is rare enough that
/// nothing had hit it in a week of production.
pub fn open_write(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(root.join("recall.sqlite"))?;
    conn.busy_timeout(Duration::from_secs(30))?;
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

/// Why a term was not added.
///
/// ⚠ **A blank term and a broken database are not the same answer.** Collapsing
/// them — which this did — told the user "vocabulary term must not be blank",
/// with a 400, when the truth was a locked or unwritable `recall.sqlite`. The
/// caller then retypes a term that was never the problem, and the real fault is
/// invisible because a 400 is not a fault anyone investigates.
#[derive(Debug)]
pub enum TermError {
    /// Nothing but whitespace was sent.
    Blank,
    /// The database refused the write.
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for TermError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
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
    )?;
    Ok(conn.query_row(
        "SELECT id FROM vocabulary WHERE term = ?1",
        [cleaned],
        |r| r.get(0),
    )?)
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

/// An ISO-8601 instant, or a 400. Never a substituted "now".
///
/// ⚠ Manufacturing a time for a malformed one would queue a refine over the wrong
/// stretch of audio — silently, since the request would succeed.
///
/// ⚠ Spelled by [`crate::instant`], not by chrono's default. This used to
/// convert to UTC and let chrono trim the fraction, which stored a different
/// text for the same moment than every row the Python wrote.
fn instant(value: &str) -> Option<String> {
    crate::instant::python_isoformat(value)
}

pub async fn vocabulary_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("vocabulary", move || vocabulary(&reads::open(&root)?)).await
}

pub async fn vocabulary_add_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<TermIn>,
) -> Response {
    let root = st.root.clone();
    let now = chrono::Utc::now().to_rfc3339();
    let added =
        tokio::task::spawn_blocking(move || add_term(&open_write(&root)?, &body.term, &now));
    match added.await {
        Ok(Ok(id)) => Json(NewId { new_id: id }).into_response(),
        // The only answer the caller can act on: they sent whitespace.
        Ok(Err(TermError::Blank)) => {
            (StatusCode::BAD_REQUEST, "vocabulary term must not be blank").into_response()
        }
        Ok(Err(TermError::Db(err))) => route::faulted("vocabulary add", &err),
        Err(err) => route::faulted("vocabulary add task", &err),
    }
}

pub async fn vocabulary_delete_route(
    State(st): State<Arc<reads::State>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let root = st.root.clone();
    match route::blocking("vocabulary delete", move || {
        delete_term(&open_write(&root)?, id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn refine_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<RefineIn>,
) -> Response {
    let (Some(start), Some(end)) = (instant(&body.start), instant(&body.end)) else {
        return (StatusCode::BAD_REQUEST, "start and end must be ISO-8601").into_response();
    };
    let root = st.root.clone();
    let now = chrono::Utc::now().to_rfc3339();
    match route::blocking("refine enqueue", move || {
        add_refine_request(&open_write(&root)?, &body.source, &start, &end, &now)
    })
    .await
    {
        Ok(_) => route::ack(),
        Err(response) => response,
    }
}

// --- the Mac-initiated job queue (`/sync/jobs`) -------------------------------

/// A unit of work the fleet hands the Mac, which holds the ML and the mic.
///
/// ⚠ The wire names are `snake_case` — `sample_rate`, not `sampleRate`. The Python
/// model declares them bare and the Mac parses them bare.
///
/// Fields outside a job's own type are `None` and are omitted by neither side:
/// an older fleet simply never sends them.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct JobOut {
    pub id: i64,
    /// `refine` — `id` is a refine-request id. `upload` — `id` is the fleet's
    /// audio-segment id, and the upload-only fields carry what the Mac needs to
    /// bring the session home without re-probing it.
    pub r#type: String,
    pub source: String,
    pub start: Option<String>,
    pub end: Option<String>,
    pub file: Option<String>,
    pub title: Option<String>,
    pub sample_rate: Option<i64>,
    pub channels: Option<i64>,
}

/// The queue the Mac polls: interactive refines first, then uploaded sessions.
///
/// ⚠ The two share ONE limit and refines take it first. A backlog of uploads
/// must not starve a refine somebody is waiting on in the UI.
pub fn pending_jobs(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<JobOut>> {
    let mut jobs = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, source_id, start_utc, end_utc FROM refine_requests \
         WHERE done_utc IS NULL ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit], |r| {
        Ok(JobOut {
            id: r.get(0)?,
            r#type: "refine".to_owned(),
            source: r.get(1)?,
            start: r.get::<_, Option<String>>(2)?,
            end: r.get::<_, Option<String>>(3)?,
            file: None,
            title: None,
            sample_rate: None,
            channels: None,
        })
    })?;
    for row in rows {
        jobs.push(row?);
    }

    let remaining = limit - i64::try_from(jobs.len()).unwrap_or(limit);
    if remaining <= 0 {
        return Ok(jobs);
    }
    // Derived from the segment rows themselves, so nothing has to remember to
    // enqueue; `done` is `mark_transcribed`.
    let mut stmt = conn.prepare(
        "SELECT a.id, a.source_id, s.name, a.path, a.start_utc, a.end_utc, \
                a.sample_rate, a.channels \
         FROM audio_segments a JOIN sources s ON s.id = a.source_id \
         WHERE s.kind = 'upload' AND a.transcribed_utc IS NULL \
         ORDER BY a.start_utc LIMIT ?1",
    )?;
    let rows = stmt.query_map([remaining], |r| {
        let path: String = r.get(3)?;
        Ok(JobOut {
            id: r.get(0)?,
            r#type: "upload".to_owned(),
            source: r.get(1)?,
            start: r.get::<_, Option<String>>(4)?,
            end: r.get::<_, Option<String>>(5)?,
            // The BASENAME, not the stored path: the Mac fetches it by name
            // through /sync/audio/file, which refuses a path component.
            file: Some(
                std::path::Path::new(&path)
                    .file_name()
                    .map_or(path.clone(), |n| n.to_string_lossy().into_owned()),
            ),
            title: r.get::<_, Option<String>>(2)?,
            sample_rate: r.get::<_, Option<i64>>(6)?,
            channels: r.get::<_, Option<i64>>(7)?,
        })
    })?;
    for row in rows {
        jobs.push(row?);
    }
    Ok(jobs)
}

/// Retire a refine request.
pub fn mark_refine_done(conn: &Connection, id: i64, now: DateTime<Utc>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE refine_requests SET done_utc = ?1 WHERE id = ?2",
        rusqlite::params![crate::instant::python_isoformat_utc(now), id],
    )?;
    Ok(())
}

/// Retire an uploaded segment: the Mac holds it and will transcribe it.
///
/// ⚠ **`transcribed_utc` is set to the segment's own `end_utc`, NOT to now.**
/// That is deliberate and load-bearing: the column reads as "the recording this
/// covers ended then", so anything ordering or ageing by it stays on the
/// RECORDING's clock rather than on when a machine got round to it. Writing
/// `now` here makes a months-old backlog look like it was all recorded today.
pub fn mark_transcribed(conn: &Connection, audio_segment_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE audio_segments SET transcribed_utc = end_utc WHERE id = ?1",
        [audio_segment_id],
    )?;
    Ok(())
}

// --- the instant feed (`POST /sync/live`) -------------------------------------

/// One provisional live turn the Mac pushes for the fleet's instant feed.
#[derive(Debug, Deserialize)]
pub struct LiveTurn {
    pub start: String,
    pub end: String,
    pub text: String,
    pub asr_model: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// Persist pushed live turns — audio-less provisional transcripts shown at once
/// while the archive pass catches up, then reconciled when the segment spanning
/// them arrives.
///
/// ⚠ **Idempotent by (model, start, text).** A retried push, or one the archive
/// has already reconciled to hidden, is SKIPPED — so a re-push never duplicates a
/// turn and never resurrects a hidden one. The presence check deliberately asks
/// for the literal `live` model rather than the turn's own, matching the Python:
/// the feed is what is being deduplicated, not whatever produced it.
///
/// ⚠ **The search index is maintained in CODE.** `transcript_fts` is a
/// contentless FTS5 table with no trigger behind it; forgetting the second insert
/// fails nothing and quietly makes every live turn unfindable by search.
///
/// One transaction per turn, so a turn and its index row land together or not at
/// all — a half-written pair would be a turn that exists and cannot be found.
pub fn ingest_live(conn: &mut Connection, turns: &[LiveTurn]) -> rusqlite::Result<usize> {
    let mut stored = 0;
    for turn in turns {
        // The stored spelling, so the presence check and the insert agree. A
        // turn re-spelled on the way in would never match its own earlier copy.
        let Some(start) = crate::instant::python_isoformat(&turn.start) else {
            continue;
        };
        let Some(end) = crate::instant::python_isoformat(&turn.end) else {
            continue;
        };
        let present: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM transcript_segments \
                 WHERE asr_model = 'live' AND start_utc = ?1 AND text = ?2 LIMIT 1",
                rusqlite::params![start, turn.text],
                |r| r.get(0),
            )
            .optional()?;
        if present.is_some() {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO transcript_segments \
             (audio_segment_id, start_utc, end_utc, text, language, asr_model) \
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![start, end, turn.text, turn.language, turn.asr_model],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
            rusqlite::params![id, turn.text],
        )?;
        tx.commit()?;
        stored += 1;
    }
    Ok(stored)
}
