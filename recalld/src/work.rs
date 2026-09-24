//! The meaning plane's writers that are not a pass: vocabulary, and the
//! instant feed's turns.
//!
//! Vocabulary is the proper nouns fed to Whisper as `initial_prompt` on every
//! pass; the runner refuses to transcribe without it.

use crate::{reads, route};
use audiocore::instant;
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
/// Separate from [`reads::open`], so a bug in a read path cannot write. The
/// 30 s busy timeout lets several writers share the file: at 5 s, contended
/// requests answered 500 `database is locked`, and a late correct answer beats
/// a prompt failure.
pub fn open_write(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(root.join("recall.sqlite"))?;
    conn.busy_timeout(Duration::from_secs(30))?;
    Ok(conn)
}

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, rename = "VocabularyTerm")]
pub struct Term {
    pub id: i64,
    pub term: String,
}

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, rename = "VocabularyList")]
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
/// A blank term (400, the caller's to fix) and a database failure (500) are
/// kept apart, so a locked database is not reported as bad input.
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
/// Idempotent: `ON CONFLICT DO NOTHING`, then read the id back, because the
/// Labels page cannot know what is already in the list before it posts.
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

#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "VocabularyRequest")]
pub struct TermIn {
    term: String,
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
    let now = audiocore::instant::python_isoformat_utc(chrono::Utc::now());
    let added =
        tokio::task::spawn_blocking(move || add_term(&open_write(&root)?, &body.term, &now));
    match added.await {
        Ok(Ok(id)) => Json(route::NewId { new_id: id }).into_response(),
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
/// `start_utc` is where in the audio the words were said; `created_utc` is when
/// this tier delivered them, the only record of its latency.
///
/// Idempotent by (start, text) among `live` turns: a retried push, or one the
/// archive has already reconciled to hidden, is skipped, so a re-push never
/// duplicates a turn or resurrects a hidden one.
///
/// Degenerate text is dropped here rather than by each pusher: Whisper loops on
/// the short, hard clips this tier is made of ("goog goog goog…"), and that is a
/// property of the string. Dropped turns are not counted as stored.
pub fn ingest_live(
    conn: &mut Connection,
    turns: &[LiveTurn],
    now: DateTime<Utc>,
) -> rusqlite::Result<usize> {
    let delivered = instant::python_isoformat_utc(now);
    // Read once, here rather than passed in: the same names biased the ASR
    // prompt, so the refusal and its cause cannot drift apart.
    let names = crate::labels::known_speaker_names(conn)?.names;
    let mut stored = 0;
    for turn in turns {
        // The stored spelling, so the presence check and the insert agree. A
        // turn re-spelled on the way in would never match its own earlier copy.
        let Some(start) = instant::parse_utc(&turn.start).map(instant::python_isoformat_utc) else {
            continue;
        };
        let Some(end) = instant::parse_utc(&turn.end).map(instant::python_isoformat_utc) else {
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
        crate::turn_store::insert(
            &tx,
            &crate::turn_store::NewTurn {
                start_utc: &start,
                end_utc: &end,
                text: &turn.text,
                language: turn.language.as_deref(),
                asr_model: Some(&turn.asr_model),
                created_utc: Some(&delivered),
                ..crate::turn_store::NewTurn::default()
            },
        )?;
        tx.commit()?;
        stored += 1;
    }
    Ok(stored)
}
