//! Meaning-plane writers that are not a pass: the vocabulary (proper nouns fed
//! to Whisper as `initial_prompt`) and the live feed's turns.

use crate::{reads, route};
use audiocore::instant::Stamp;
use audiocore::work::LiveTurn;
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
        "INSERT OR IGNORE INTO vocabulary (term, created_utc, owner) VALUES (?1, ?2, ?3)";
    TERM_ID: Meaning =
        "SELECT id FROM vocabulary WHERE term = ?1 AND owner IS ?2";
    DELETE_TERM: Meaning =
        "DELETE FROM vocabulary WHERE id = ?1";
    LIVE_TURN_EXISTS: Meaning =
        "SELECT 1 FROM live_lines WHERE start_utc = ?1 AND text = ?2 LIMIT 1";
    ADD_LIVE: Meaning =
        "INSERT INTO live_lines (start_utc, end_utc, text, language, asr_model, created_utc)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
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

/// Add a term to `owner`'s glossary, returning its id; idempotent.
pub fn add_term(
    conn: &Connection,
    owner: Option<&str>,
    term: &str,
    now: &Stamp,
) -> Result<i64, TermError> {
    let cleaned = term.trim();
    if cleaned.is_empty() {
        return Err(TermError::Blank);
    }
    ADD_TERM.execute(conn, (cleaned, now, owner))?;
    Ok(TERM_ID.query_row(conn, (cleaned, owner), |r| r.get(0))?)
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

/// The asker's own glossary.
pub async fn vocabulary_route(
    State(st): State<Arc<reads::State>>,
    asked: crate::webauth::Asked,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    route::json("vocabulary", move || {
        vocabulary(&reads::open_as(&root, viewer.as_ref())?)
    })
    .await
}

/// A term for the asker's own glossary.
pub async fn vocabulary_add_route(
    State(st): State<Arc<reads::State>>,
    asked: crate::webauth::Asked,
    Json(body): Json<TermIn>,
) -> Response {
    let owner = crate::webauth::viewer(asked).map(|v| v.user_id);
    let root = st.root.clone();
    let now = audiocore::instant::Stamp::now();
    let added = tokio::task::spawn_blocking(move || {
        add_term(&open_write(&root)?, owner.as_deref(), &body.term, &now)
    });
    match added.await {
        Ok(Ok(id)) => Json(route::NewId { new_id: id }).into_response(),
        Ok(Err(TermError::Blank)) => {
            (StatusCode::BAD_REQUEST, "vocabulary term must not be blank").into_response()
        }
        Ok(Err(TermError::Db(err))) => route::faulted("vocabulary add", &err),
        Err(err) => route::faulted("vocabulary add task", &err),
    }
}

/// Only the asker's own term.
pub async fn vocabulary_delete_route(
    State(st): State<Arc<reads::State>>,
    asked: crate::webauth::Asked,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    if let Err(refused) =
        reads::check(&st.root, viewer.as_ref(), vec![reads::Target::Term(id)]).await
    {
        return refused;
    }
    let root = st.root.clone();
    route::acked(
        route::blocking("vocabulary delete", move || {
            delete_term(&open_write(&root)?, id)
        })
        .await,
    )
}

// --- the live feed (`POST /sync/live`) ----------------------------------------

/// Store pushed live turns: provisional, audio-less lines the view shows until
/// a transcribed clip covers their start. `created_utc` records the tier's
/// latency.
///
/// Idempotent by (start, text), so a retried push does not duplicate a turn.
/// Loops, wordless text and bare names are dropped and not counted.
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
        ADD_LIVE.execute(
            conn,
            rusqlite::params![
                start,
                end,
                turn.text,
                turn.language,
                turn.asr_model,
                delivered
            ],
        )?;
        stored += 1;
    }
    Ok(stored)
}
