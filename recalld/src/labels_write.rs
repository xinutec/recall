//! The labelling WRITES, ported from `recall.api_labels`.
//!
//! ⚠ **These touch the one thing in the product that cannot be re-derived.**
//! Turns, tiers and attribution are all recomputable from audio; a name a person
//! typed is not. Everything here changes `speaker_label` on a live turn or the
//! corrections corpus, so each multi-statement write is wrapped in an EXPLICIT
//! transaction. The Python was already atomic — `sqlite3` opens an implicit
//! transaction before DML and its `_commit` ends it — but it read as three loose
//! statements, and the property a reader has to check is that a correction's
//! speaker and the live turn it produced can never disagree.
//!
//! ⚠ **A label is not display-only.** `speaker_label` is the work-list the
//! voiceprint backfill selects on, so naming a voice enrols it. That is why
//! re-assigning a correction also DROPS its embedding: the clip has to be
//! re-enrolled under the new name rather than keep matching the old one.

use crate::route;
use rusqlite::{Connection, Transaction};

/// `asr_model` of a turn a person authored.
const HUMAN_MODEL: &str = "human";
/// Why a correction was hidden from the corpus by a human in review.
const HIDE_REASON: &str = "review";

/// Provenance stamped on the human turn that replaced `original_id`.
///
/// ⚠ Written by the correction path and MATCHED here to find that live turn
/// again. The two spellings must agree exactly or a re-assignment updates the
/// corpus row and silently leaves the turn the timeline shows under its old
/// name.
fn human_correction_provenance(original_id: i64) -> String {
    format!("human correction of #{original_id}")
}

/// Set or clear the human speaker on one turn.
///
/// Display label only: no correction pair is recorded and no voiceprint changes,
/// because this fixes a turn diarization put on the wrong voice rather than
/// asserting anything about the words.
pub fn set_turn_speaker(
    conn: &Connection,
    segment_id: i64,
    name: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = ?1 WHERE id = ?2",
        (name, segment_id),
    )
}

fn drop_voiceprint(tx: &Transaction, correction_id: i64) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM speaker_embeddings WHERE source_correction_id = ?1",
        [correction_id],
    )?;
    Ok(())
}

/// Re-assign a correction's voice: the corpus pair, the live turn it produced,
/// and its voiceprint.
///
/// ⚠ All three or none. Updating the pair without the turn leaves the timeline
/// showing the old name; dropping the embedding without the pair re-enrols the
/// clip under the name that was just found to be wrong.
pub fn set_correction_speaker(
    conn: &mut Connection,
    correction_id: i64,
    speaker: &str,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let original: Option<i64> = tx
        .query_row(
            "SELECT transcript_segment_id FROM corrections WHERE id = ?1",
            [correction_id],
            |r| r.get(0),
        )
        .ok();
    tx.execute(
        "UPDATE corrections SET speaker = ?1 WHERE id = ?2",
        (speaker, correction_id),
    )?;
    if let Some(original) = original {
        tx.execute(
            "UPDATE transcript_segments SET speaker_label = ?1 \
             WHERE provenance = ?2 AND asr_model = ?3 AND superseded_by IS NULL",
            (speaker, human_correction_provenance(original), HUMAN_MODEL),
        )?;
    }
    drop_voiceprint(&tx, correction_id)?;
    tx.commit()
}

/// Soft-remove a bad label from the corpus, the counts, and the matching pool.
///
/// ⚠ Hidden, not deleted: the pair is evidence that a person read this clip and
/// judged it, which is worth keeping even when the judgement was that the clip
/// is unusable.
pub fn hide_correction(conn: &mut Connection, correction_id: i64) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE corrections SET hidden_reason = ?1 WHERE id = ?2",
        (HIDE_REASON, correction_id),
    )?;
    drop_voiceprint(&tx, correction_id)?;
    tx.commit()
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, work};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct TurnSpeakerIn {
    name: Option<String>,
}

#[derive(Deserialize)]
pub struct ReassignIn {
    speaker: String,
}

/// An empty name CLEARS the label rather than storing "", which would render as
/// a speaker whose name is nothing.
fn cleaned(name: Option<String>) -> Option<String> {
    name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty())
}

pub async fn turn_speaker_route(
    State(st): State<Arc<reads::State>>,
    Path(segment_id): Path<i64>,
    Json(body): Json<TurnSpeakerIn>,
) -> Response {
    let root = st.root.clone();
    let name = cleaned(body.name);
    match route::blocking("turn speaker", move || {
        set_turn_speaker(&work::open_write(&root)?, segment_id, name.as_deref())
    })
    .await
    {
        Ok(_) => route::ack(),
        Err(response) => response,
    }
}

pub async fn correction_reassign_route(
    State(st): State<Arc<reads::State>>,
    Path(correction_id): Path<i64>,
    Json(body): Json<ReassignIn>,
) -> Response {
    let speaker = body.speaker.trim().to_owned();
    if speaker.is_empty() {
        // Clearing a correction's speaker is not a gesture the UI offers, and an
        // empty one would drop the voiceprint for a name nobody chose.
        return (StatusCode::BAD_REQUEST, "speaker required").into_response();
    }
    let root = st.root.clone();
    match route::blocking("correction reassign", move || {
        let mut conn = work::open_write(&root)?;
        set_correction_speaker(&mut conn, correction_id, &speaker)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

pub async fn correction_hide_route(
    State(st): State<Arc<reads::State>>,
    Path(correction_id): Path<i64>,
) -> Response {
    let root = st.root.clone();
    match route::blocking("correction hide", move || {
        let mut conn = work::open_write(&root)?;
        hide_correction(&mut conn, correction_id)
    })
    .await
    {
        Ok(()) => route::ack(),
        Err(response) => response,
    }
}

// --- the correction itself ---------------------------------------------------

/// A human turn's ASR confidence. Not a score: a person read it.
const HUMAN_CONFIDENCE: f64 = 1.0;

/// The columns a correction carries forward from the turn it replaces.
struct Original {
    id: i64,
    audio_segment_id: Option<i64>,
    start_utc: String,
    end_utc: String,
    text: String,
    language: Option<String>,
    language_confidence: Option<f64>,
    asr_confidence: Option<f64>,
    speaker_label: Option<String>,
    speaker_id: Option<i64>,
    speaker_cluster: Option<String>,
    superseded_by: Option<i64>,
}

/// Why a correction was refused.
#[derive(Debug)]
pub enum CorrectError {
    /// Nothing but whitespace was typed.
    Blank,
    /// No turn with that id.
    Missing(i64),
    /// That turn has already been replaced.
    AlreadySuperseded(i64),
    /// The overridden start or end is not an ISO-8601 instant.
    BadSpan,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for CorrectError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// What the caller may override when correcting a turn.
#[derive(Debug, Default)]
pub struct Correction<'a> {
    /// Who said it. `None` keeps whatever the turn had.
    pub speaker: Option<&'a str>,
    /// A tighter span, from the boundary editor trimming a clip to one speaker.
    pub start: Option<&'a str>,
    pub end: Option<&'a str>,
    /// A mis-detected language, e.g. Dutch heard as English.
    pub language: Option<&'a str>,
}

fn load_original(tx: &Transaction, segment_id: i64) -> Result<Original, CorrectError> {
    tx.query_row(
        "SELECT id, audio_segment_id, start_utc, end_utc, text, language, \
                language_confidence, asr_confidence, speaker_label, speaker_id, \
                speaker_cluster, superseded_by \
         FROM transcript_segments WHERE id = ?1",
        [segment_id],
        |r| {
            Ok(Original {
                id: r.get(0)?,
                audio_segment_id: r.get(1)?,
                start_utc: r.get(2)?,
                end_utc: r.get(3)?,
                text: r.get(4)?,
                language: r.get(5)?,
                language_confidence: r.get(6)?,
                asr_confidence: r.get(7)?,
                speaker_label: r.get(8)?,
                speaker_id: r.get(9)?,
                speaker_cluster: r.get(10)?,
                superseded_by: r.get(11)?,
            })
        },
    )
    .map_err(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => CorrectError::Missing(segment_id),
        other => CorrectError::Db(other),
    })
}

/// Replace a turn with a human-authored one and record the corpus pair.
///
/// ⚠ **One transaction, unlike the Python.** `apply_correction` called three
/// store methods that each committed, so a failure between them could leave a
/// human turn superseding nothing, or an original superseded with no pair to
/// show what it became. Here the new turn, its search-index row, the supersede
/// and the pair land together or not at all.
///
/// ⚠ **The search index is maintained in CODE, not by a trigger.** `transcript_fts`
/// is a contentless FTS5 table that the writer inserts into by hand. Forgetting
/// it here would not fail anything — it would just make every human correction
/// unfindable by search, which is the one thing a corrected turn is most likely
/// to be looked up by.
pub fn apply_correction(
    conn: &mut Connection,
    segment_id: i64,
    corrected_text: &str,
    now: &str,
    edit: &Correction,
) -> Result<i64, CorrectError> {
    let text = corrected_text.trim();
    if text.is_empty() {
        return Err(CorrectError::Blank);
    }
    let tx = conn.transaction()?;
    let old = load_original(&tx, segment_id)?;
    if old.superseded_by.is_some() {
        // A double-tap, or a second tab correcting a stale id, would mint a
        // SECOND current human turn and a duplicate corpus pair.
        return Err(CorrectError::AlreadySuperseded(segment_id));
    }
    let language = edit.language.map(str::to_owned).or(old.language);
    // ⚠ An overridden span is RE-SPELLED, not stored as sent. These columns are
    // compared as text, so a client sending `...01Z` where the table holds
    // `...01+00:00` would write a turn that sorts into the wrong page. See
    // `crate::instant`.
    let start = match edit.start {
        Some(value) => crate::instant::python_isoformat(value).ok_or(CorrectError::BadSpan)?,
        None => old.start_utc.clone(),
    };
    let end = match edit.end {
        Some(value) => crate::instant::python_isoformat(value).ok_or(CorrectError::BadSpan)?,
        None => old.end_utc.clone(),
    };
    let (start, end) = (start.as_str(), end.as_str());
    // A speaker given here wins; otherwise the turn keeps the name it had.
    let speaker_label = edit.speaker.map(str::to_owned).or(old.speaker_label);

    tx.execute(
        "INSERT INTO transcript_segments \
            (audio_segment_id, start_utc, end_utc, text, language, language_confidence, \
             asr_confidence, asr_model, speaker_label, speaker_id, speaker_cluster, \
             provenance, created_utc) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            old.audio_segment_id,
            start,
            end,
            text,
            language,
            old.language_confidence,
            HUMAN_CONFIDENCE,
            HUMAN_MODEL,
            speaker_label,
            old.speaker_id,
            // Carried forward so a corrected turn stays attributed to its voice
            // instead of falling back to unknown.
            old.speaker_cluster,
            human_correction_provenance(old.id),
            now,
        ],
    )?;
    let new_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (new_id, text),
    )?;
    tx.execute(
        "UPDATE transcript_segments SET superseded_by = ?1 WHERE id = ?2",
        (new_id, old.id),
    )?;
    tx.execute(
        "INSERT INTO corrections \
            (transcript_segment_id, audio_segment_id, start_utc, end_utc, \
             original_text, corrected_text, language, created_utc, speaker, \
             audio_confidence) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        rusqlite::params![
            old.id,
            old.audio_segment_id,
            start,
            end,
            old.text,
            text,
            language,
            now,
            edit.speaker,
            // The clip's audio quality, carried onto the pair: a readable label
            // on faint audio is still good ASR data but too degraded to enrol.
            old.asr_confidence,
        ],
    )?;
    tx.commit()?;
    Ok(new_id)
}

#[derive(Deserialize)]
pub struct CorrectIn {
    id: i64,
    text: String,
    speaker: Option<String>,
    start: Option<String>,
    end: Option<String>,
    language: Option<String>,
}

#[derive(serde::Serialize)]
struct NewId {
    #[serde(rename = "newId")]
    new_id: i64,
}

pub async fn correct_route(
    State(st): State<Arc<reads::State>>,
    Json(body): Json<CorrectIn>,
) -> Response {
    let root = st.root.clone();
    let now = chrono::Utc::now().to_rfc3339();
    let applied = tokio::task::spawn_blocking(move || {
        let mut conn = work::open_write(&root)?;
        apply_correction(
            &mut conn,
            body.id,
            &body.text,
            &now,
            &Correction {
                speaker: body.speaker.as_deref(),
                start: body.start.as_deref(),
                end: body.end.as_deref(),
                language: body.language.as_deref(),
            },
        )
    });
    match applied.await {
        Ok(Ok(new_id)) => Json(NewId { new_id }).into_response(),
        // These three are the caller's to fix, and each says which.
        Ok(Err(CorrectError::Blank)) => {
            (StatusCode::BAD_REQUEST, "corrected text must not be blank").into_response()
        }
        Ok(Err(CorrectError::Missing(id))) => (
            StatusCode::BAD_REQUEST,
            format!("no transcript segment with id {id}"),
        )
            .into_response(),
        Ok(Err(CorrectError::BadSpan)) => {
            (StatusCode::BAD_REQUEST, "start and end must be ISO-8601").into_response()
        }
        Ok(Err(CorrectError::AlreadySuperseded(id))) => (
            StatusCode::BAD_REQUEST,
            format!("turn #{id} was already superseded - correct the current version"),
        )
            .into_response(),
        Ok(Err(CorrectError::Db(err))) => route::faulted("correct", &err),
        Err(err) => route::faulted("correct task", &err),
    }
}
