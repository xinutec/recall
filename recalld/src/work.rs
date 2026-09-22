//! The meaning plane's writers that are not a pass: vocabulary, and the
//! instant feed's turns.
//!
//! Vocabulary is the household's proper nouns, fed to Whisper as
//! `initial_prompt` on every pass; the runner refuses to transcribe without it.

use crate::{reads, route};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Open `recall.sqlite` for writing.
///
/// Separate from [`reads::open`]: a read route keeps the read-only handle so a
/// bug in a read path cannot write. The busy timeout is what makes several
/// writers civil; at 5 s a mirror handshake under contention answered 500
/// `database is locked`, at 30 s it waits. `GET /api/capture` takes this handle
/// too, and a late correct answer beats a prompt failure there.
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

// --- HTTP ------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TermIn {
    term: String,
}

#[derive(Serialize)]
struct NewId {
    #[serde(rename = "newId")]
    new_id: i64,
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
/// ⚠ **`start_utc` is where in the AUDIO the words were said; `created_utc` is
/// when this tier delivered them.** Only the second can show a stall after the
/// fact, and live turns carried no delivery instant at all until #1383 — so the
/// tier whose entire value is immediacy left no evidence of its own latency.
///
/// Idempotent by (model, start, text): a retried push, or one the archive has
/// already reconciled to hidden, is skipped, so a re-push never duplicates a
/// turn and never resurrects a hidden one.
///
/// ⚠ **Degenerate text is dropped HERE, not by the pusher.** Whisper loops on
/// the short, hard clips this tier is made of ("goog goog goog…"), and whether
/// a string is a model artefact is a property of the string — so it belongs
/// where the row is written, once, rather than in each agent that might push
/// one. Dropped turns are not counted as stored.
///
/// ⚠ **The search index is maintained in CODE.** `transcript_fts` is a
/// contentless FTS5 table with no trigger behind it; forgetting the second insert
/// fails nothing and quietly makes every live turn unfindable by search.
///
/// One transaction per turn, so a turn and its index row land together or not at
/// all — a half-written pair would be a turn that exists and cannot be found.
pub fn ingest_live(
    conn: &mut Connection,
    turns: &[LiveTurn],
    now: DateTime<Utc>,
) -> rusqlite::Result<usize> {
    let delivered = now.to_rfc3339_opts(SecondsFormat::Micros, false);
    // ⚠ Read ONCE, not per turn, and read here rather than passed in: the names
    // are the same list the ASR prompt biased the model with, so the refusal and
    // the cause cannot drift apart.
    let names = crate::labels::known_speaker_names(conn)?.names;
    let mut stored = 0;
    for turn in turns {
        // The stored spelling, so the presence check and the insert agree. A
        // turn re-spelled on the way in would never match its own earlier copy.
        let Some(start) = audiocore::instant::python_isoformat(&turn.start) else {
            continue;
        };
        let Some(end) = audiocore::instant::python_isoformat(&turn.end) else {
            continue;
        };
        if crate::quality::is_repetition_loop(&turn.text)
            || crate::quality::is_wordless(&turn.text)
            || crate::quality::is_bare_name(&turn.text, &names)
        {
            continue;
        }
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
             (audio_segment_id, start_utc, end_utc, text, language, asr_model, created_utc) \
             VALUES (NULL, ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                start,
                end,
                turn.text,
                turn.language,
                turn.asr_model,
                delivered
            ],
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
