//! Meaning-plane writers that are not a pass: the vocabulary (proper nouns fed
//! to Whisper as `initial_prompt`) and the live feed's turns.

use crate::{reads, route};
use audiocore::instant::Stamp;
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

crate::statements! {
    VOCABULARY: Meaning =
        "SELECT id, term FROM vocabulary ORDER BY term COLLATE NOCASE";
    ADD_TERM: Meaning =
        "INSERT INTO vocabulary (term, created_utc) VALUES (?1, ?2) ON CONFLICT(term) DO NOTHING";
    TERM_ID: Meaning =
        "SELECT id FROM vocabulary WHERE term = ?1";
    DELETE_TERM: Meaning =
        "DELETE FROM vocabulary WHERE id = ?1";
    LIVE_TURN_EXISTS: Meaning =
        "SELECT 1 FROM transcript_segments \
                 WHERE asr_model = 'live' AND start_utc = ?1 AND text = ?2 LIMIT 1";
}

/// Open `recall.sqlite` for writing (apart from [`reads::open`], so a read
/// path cannot write). At a 5 s busy timeout contended requests answered 500.
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

/// The terms, case-insensitively ordered.
pub fn vocabulary(conn: &Connection) -> rusqlite::Result<VocabularyOut> {
    let mut stmt = VOCABULARY.prepare(conn)?;
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

/// A blank term (400) apart from a database failure (500).
#[derive(Debug)]
pub enum TermError {
    Blank,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for TermError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// Add a term, returning its id; idempotent.
pub fn add_term(conn: &Connection, term: &str, now: &Stamp) -> Result<i64, TermError> {
    let cleaned = term.trim();
    if cleaned.is_empty() {
        return Err(TermError::Blank);
    }
    ADD_TERM.execute(conn, (cleaned, now))?;
    Ok(TERM_ID.query_row(conn, [cleaned], |r| r.get(0))?)
}

pub fn delete_term(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    DELETE_TERM.execute(conn, [id])?;
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
    let now = audiocore::instant::Stamp::now();
    let added =
        tokio::task::spawn_blocking(move || add_term(&open_write(&root)?, &body.term, &now));
    match added.await {
        Ok(Ok(id)) => Json(route::NewId { new_id: id }).into_response(),
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

// --- the live feed (`POST /sync/live`) ----------------------------------------

#[derive(Debug, Deserialize)]
pub struct LiveTurn {
    pub start: String,
    pub end: String,
    pub text: String,
    pub asr_model: String,
    #[serde(default)]
    pub language: Option<String>,
}

/// Store pushed live turns: provisional, audio-less lines shown until the
/// archive pass writes their minute. `created_utc` records the tier's latency.
///
/// Idempotent by (start, text) among live turns, so a retried push neither
/// duplicates a turn nor brings back a reconciled one. Loops, wordless text
/// and bare names are dropped and not counted.
pub fn ingest_live(
    conn: &mut Connection,
    turns: &[LiveTurn],
    now: DateTime<Utc>,
) -> rusqlite::Result<usize> {
    let delivered = Stamp::of(now);
    // The names the prompt biased the model toward.
    let names = crate::labels::known_speaker_names(conn)?.names;
    let mut stored = 0;
    for turn in turns {
        // The stored spelling, so the presence check matches earlier copies.
        let (Some(start), Some(end)) = (Stamp::parse(&turn.start), Stamp::parse(&turn.end)) else {
            continue;
        };
        if crate::quality::is_repetition_loop(&turn.text)
            || crate::quality::is_wordless(&turn.text)
            || crate::quality::is_bare_name(&turn.text, &names)
        {
            continue;
        }
        let present: Option<i64> = LIVE_TURN_EXISTS
            .query_row(conn, rusqlite::params![start, turn.text], |r| r.get(0))
            .optional()?;
        if present.is_some() {
            continue;
        }
        let tx = crate::sql::write(conn)?;
        crate::turn_store::insert(
            &tx,
            &crate::turn_store::NewTurn {
                language: turn.language.as_deref(),
                asr_model: Some(&turn.asr_model),
                created_utc: Some(&delivered),
                ..crate::turn_store::NewTurn::at(&start, &end, &turn.text)
            },
        )?;
        tx.commit()?;
        stored += 1;
    }
    Ok(stored)
}
