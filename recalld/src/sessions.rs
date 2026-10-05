//! Uploaded sessions: one discrete recording, such as a meeting, as opposed to
//! the continuous capture. The upload itself is `crate::upload`.
//!
//! Every mutating route is guarded to uploads (404 for no such source, 400 for
//! the continuous archive).

use crate::{reads, route, work};
use audiocore::instant::Stamp;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

crate::statements! {
    /// Pin a session's language, or `NULL` to leave it to the model.
    SET_LANGUAGE: Meaning = "UPDATE sources SET language = ?2 WHERE id = ?1";
    LANGUAGE_OF: Meaning = "SELECT language FROM sources WHERE id = ?1";
    SESSION_FILES: Ingest = "SELECT filename FROM segments WHERE source = ?1 ORDER BY filename";
    SESSIONS: Meaning =
        "SELECT s.id, s.name, MIN(a.start_utc), MAX(a.end_utc), COUNT(t.id), \
                GROUP_CONCAT(DISTINCT CASE \
                    WHEN t.id IS NULL THEN NULL \
                    WHEN t.speaker_label IS NOT NULL \
                         AND t.speaker_label NOT LIKE 'SPEAKER_%' \
                         THEN t.speaker_label \
                    ELSE 'unknown' \
                END), \
                s.language \
         FROM sources s \
         JOIN audio_segments a ON a.source_id = s.id \
         LEFT JOIN transcript_segments t \
                ON t.audio_segment_id = a.id \
               AND t.superseded_by IS NULL AND t.hidden_reason IS NULL \
         WHERE s.kind = ?1 \
         GROUP BY s.id \
         ORDER BY MIN(a.start_utc) DESC";
    SOURCE_KIND: Meaning =
        "SELECT kind FROM sources WHERE id = ?1";
    RENAME: Meaning =
        "UPDATE sources SET name = ?1 WHERE id = ?2";
    CLIP_COUNT: Ingest =
        "SELECT count(*) FROM segments WHERE source = ?1";
    REQUEUE_DIARIZE: Ingest =
        "UPDATE jobs SET state = 'queued', leased_until = NULL, done_utc = NULL, result = NULL
         WHERE kind = ?1 AND done_utc IS NOT NULL
           AND filename IN (SELECT filename FROM segments WHERE source = ?2)";
    CLEAR_DIARIZE_LEDGER: Ingest =
        "DELETE FROM pass_ledger WHERE kind = ?1
           AND filename IN (SELECT filename FROM segments WHERE source = ?2)";
    SESSION_TURNS: Meaning =
        "SELECT t.start_utc, t.text, t.speaker_label, t.speaker_cluster \
         FROM transcript_segments t \
         JOIN audio_segments a ON a.id = t.audio_segment_id \
         WHERE a.source_id = ?1 AND t.superseded_by IS NULL \
           AND t.hidden_reason IS NULL \
         ORDER BY t.start_utc";
    SESSION_AUDIO: Meaning =
        "SELECT id, path, start_utc FROM audio_segments WHERE source_id = ?1";
    REMEMBER_DELETED: Meaning =
        "INSERT OR IGNORE INTO deleted_segments (source_id, start_utc, deleted_utc) \
             VALUES (?1, ?2, ?3)";
    CLIP_TURNS: Meaning =
        "SELECT id FROM transcript_segments WHERE audio_segment_id = ?1";
    DROP_TURN_EMBEDDING: Meaning =
        "DELETE FROM transcript_embeddings WHERE segment_id = ?1";
    DROP_CORRECTIONS: Meaning =
        "DELETE FROM corrections WHERE audio_segment_id = ?1";
    DROP_REFINE_REQUESTS: Meaning =
        "DELETE FROM refine_requests WHERE source_id = ?1";
    DROP_AUDIO: Meaning =
        "DELETE FROM audio_segments WHERE source_id = ?1";
    DROP_SOURCE: Meaning =
        "DELETE FROM sources WHERE id = ?1";
}

const UPLOAD_KIND: &str = "upload";

/// A diarization tag, not a person's name; never listed as a speaker.
const CLUSTER_PREFIX: &str = "SPEAKER";

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, rename = "Session")]
#[serde(rename_all = "camelCase")]
pub struct SessionOut {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub turn_count: i64,
    pub speakers: Vec<String>,
    /// The language every transcription of it uses; `None`: the model guesses.
    pub language: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[ts(export, rename = "SessionList")]
pub struct SessionsOut {
    pub items: Vec<SessionOut>,
}

/// Every uploaded session, newest first. Only names a person gave are listed
/// as speakers; guesses have no trustworthy threshold.
pub fn sessions(conn: &Connection) -> rusqlite::Result<SessionsOut> {
    let mut stmt = SESSIONS.prepare(conn)?;
    let rows = stmt.query_map([UPLOAD_KIND], |r| {
        let names: Option<String> = r.get(5)?;
        // GROUP_CONCAT's order is undefined.
        let mut speakers: Vec<String> = names
            .filter(|s| !s.is_empty())
            .map(|s| s.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        speakers.sort();
        Ok(SessionOut {
            id: r.get(0)?,
            title: r.get(1)?,
            start: r.get(2)?,
            end: r.get(3)?,
            turn_count: r.get(4)?,
            speakers,
            language: r.get(6)?,
        })
    })?;
    Ok(SessionsOut {
        items: rows.collect::<rusqlite::Result<_>>()?,
    })
}

#[derive(Debug)]
pub enum SessionError {
    Missing,
    /// The continuous archive, not an upload.
    NotAnUpload,
    NoAudio,
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for SessionError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

impl SessionError {
    fn into_response(self, what: &str) -> Response {
        match self {
            Self::Missing => (StatusCode::NOT_FOUND, "no such session").into_response(),
            Self::NotAnUpload => {
                (StatusCode::BAD_REQUEST, "not an uploaded session").into_response()
            }
            Self::NoAudio => (StatusCode::BAD_REQUEST, "session has no audio").into_response(),
            Self::Db(err) => route::faulted(what, &err),
        }
    }
}

fn require_upload(conn: &Connection, source: &str) -> Result<(), SessionError> {
    let kind: Option<String> = SOURCE_KIND.query_row(conn, [source], |r| r.get(0)).ok();
    match kind.as_deref() {
        None => Err(SessionError::Missing),
        Some(UPLOAD_KIND) => Ok(()),
        Some(_) => Err(SessionError::NotAnUpload),
    }
}

pub fn rename(conn: &Connection, source: &str, title: &str) -> Result<(), SessionError> {
    require_upload(conn, source)?;
    RENAME.execute(conn, (title, source))?;
    Ok(())
}

/// Diarize a whole session again: its finished diarize jobs are re-queued and
/// their ledger rows cleared, so the pass decides afresh. Returns how many were
/// re-queued; zero if none had finished.
pub fn rediarize(
    meaning: &Connection,
    ingest: &Connection,
    source: &str,
) -> Result<usize, SessionError> {
    require_upload(meaning, source)?;
    let clips: i64 = CLIP_COUNT.query_row(ingest, [source], |r| r.get(0))?;
    if clips == 0 {
        return Err(SessionError::NoAudio);
    }
    let tx = crate::sql::write_shared(ingest)?;
    let requeued = REQUEUE_DIARIZE.execute(&tx, (audiocore::job::Kind::DiarizeSegment, source))?;
    CLEAR_DIARIZE_LEDGER.execute(&tx, (audiocore::job::Kind::DiarizeSegment, source))?;
    tx.commit()?;
    Ok(requeued)
}

/// The household's two languages. The model guesses a third for Dutch often
/// enough (Italian, 2026-10-03) that any other pin is likely a mistake.
pub const LANGUAGES: &[&str] = &["nl", "en"];

/// Absent or empty: the model guesses. Anything outside [`LANGUAGES`] is
/// refused.
pub fn parse_language(raw: Option<&str>) -> Result<Option<&'static str>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(code) => LANGUAGES
            .iter()
            .find(|l| **l == code)
            .copied()
            .map(Some)
            .ok_or_else(|| format!("language must be one of {LANGUAGES:?} or empty, not {code:?}")),
    }
}

/// Pin (or unpin) `source`'s language and transcribe every clip again with it
/// ([`crate::retranscribe`]).
pub fn set_language(
    meaning: &Connection,
    ingest: &Connection,
    source: &str,
    language: Option<&str>,
    now: &audiocore::instant::Stamp,
) -> Result<crate::retranscribe::Requested, SessionError> {
    require_upload(meaning, source)?;
    let files: Vec<String> = {
        let mut stmt = SESSION_FILES.prepare(ingest)?;
        let rows = stmt.query_map([source], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if files.is_empty() {
        return Err(SessionError::NoAudio);
    }
    pin_language(meaning, source, language)?;
    Ok(crate::retranscribe::request(ingest, &files, now)?)
}

/// Store `source`'s language without transcribing again, for a new session.
pub fn pin_language(
    conn: &Connection,
    source: &str,
    language: Option<&str>,
) -> rusqlite::Result<()> {
    SET_LANGUAGE.execute(conn, (source, language))?;
    Ok(())
}

pub fn language_of(conn: &Connection, source: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(LANGUAGE_OF
        .query_row(conn, [source], |r| r.get(0))
        .optional()?
        .flatten())
}

/// Give a leased transcription job its session's pinned language, at lease
/// time so a change applies to the next one.
pub fn attach_language(
    root: &std::path::Path,
    job: &mut crate::queue::Job,
) -> rusqlite::Result<()> {
    if job.kind != audiocore::job::Kind::TranscribeSegment {
        return Ok(());
    }
    job.language = language_of(&crate::reads::open(root)?, &job.source)?;
    Ok(())
}

/// Name a diarized voice across a whole session, or clear it with `None`.
/// Hidden turns too, so one unhidden later is named. Naming enrols the voice.
pub fn name_voice(
    conn: &Connection,
    source: &str,
    cluster: &str,
    name: Option<&str>,
) -> rusqlite::Result<usize> {
    crate::turn_store::label_voice(conn, source, cluster, name)
}

// --- the transcript export --------------------------------------------------

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct BubbleOut {
    pub start: String,
    pub speaker: String,
    pub text: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TranscriptExportOut {
    pub session: String,
    pub date: Option<String>,
    pub speakers: Vec<String>,
    pub turns: Vec<BubbleOut>,
}

/// One turn as the export reads it.
pub struct ExportTurn {
    pub start_utc: String,
    pub text: String,
    pub speaker_label: Option<String>,
    pub speaker_cluster: Option<String>,
}

/// A person's name, else the diarized voice, else "unknown".
fn who(turn: &ExportTurn) -> String {
    turn.speaker_label
        .clone()
        .or_else(|| turn.speaker_cluster.clone())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn session_turns(conn: &Connection, source: &str) -> rusqlite::Result<Vec<ExportTurn>> {
    let mut stmt = SESSION_TURNS.prepare(conn)?;
    let rows = stmt.query_map([source], |r| {
        Ok(ExportTurn {
            start_utc: r.get(0)?,
            text: r.get(1)?,
            speaker_label: r.get(2)?,
            speaker_cluster: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// The finished transcript, consecutive same-speaker turns merged, and
/// deterministic. Times are in the machine's local zone (the pod's is UTC).
pub fn clean_transcript(source: &str, turns: &[ExportTurn]) -> TranscriptExportOut {
    let mut bubbles: Vec<BubbleOut> = Vec::new();
    let mut speakers: Vec<String> = Vec::new();
    for turn in turns {
        let speaker = who(turn);
        match bubbles.last_mut() {
            Some(last) if last.speaker == speaker => {
                format!("{} {}", last.text, turn.text)
                    .trim()
                    .clone_into(&mut last.text);
            }
            _ => bubbles.push(BubbleOut {
                start: local_iso(&turn.start_utc),
                speaker,
                text: turn.text.clone(),
            }),
        }
        if let Some(label) = &turn.speaker_label
            && !label.starts_with(CLUSTER_PREFIX)
            && !speakers.contains(label)
        {
            speakers.push(label.clone());
        }
    }
    TranscriptExportOut {
        session: source.to_owned(),
        date: bubbles.first().map(|b| b.start.clone()),
        speakers,
        turns: bubbles,
    }
}

/// A stored instant in the local zone, or unchanged if it will not parse.
fn local_iso(stored: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(stored).map_or_else(
        |_| stored.to_owned(),
        |t| {
            t.with_timezone(&chrono::Local)
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false)
        },
    )
}

// --- HTTP -------------------------------------------------------------------

#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "SessionRenameRequest")]
pub struct RenameIn {
    title: String,
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "VoiceNameRequest")]
pub struct VoiceNameIn {
    cluster: String,
    #[ts(optional = nullable)]
    name: Option<String>,
}

pub async fn sessions_route(State(st): State<Arc<reads::State>>) -> Response {
    let root = st.root.clone();
    route::json("sessions", move || sessions(&reads::open(&root)?)).await
}

pub async fn rename_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
    axum::Json(body): axum::Json<RenameIn>,
) -> Response {
    let title = body.title.trim().to_owned();
    if title.is_empty() {
        return (StatusCode::BAD_REQUEST, "title required").into_response();
    }
    let root = st.root.clone();
    match tokio::task::spawn_blocking(move || rename(&work::open_write(&root)?, &source, &title))
        .await
    {
        Ok(Ok(())) => route::ack(),
        Ok(Err(err)) => err.into_response("session rename"),
        Err(err) => route::faulted("session rename task", &err),
    }
}

pub async fn rediarize_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
) -> Response {
    let root = st.root.clone();
    match tokio::task::spawn_blocking(move || {
        let meaning = work::open_write(&root)?;
        let ingest = crate::store::open(&root)?;
        rediarize(&meaning, &ingest, &source)
    })
    .await
    {
        Ok(Ok(_)) => route::ack(),
        Ok(Err(err)) => err.into_response("session rediarize"),
        Err(err) => route::faulted("session rediarize task", &err),
    }
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(export, rename = "SessionLanguageRequest")]
pub struct LanguageIn {
    /// `"nl"`, `"en"`, or null for the model's guess.
    #[ts(optional = nullable)]
    language: Option<String>,
}

/// `POST /api/sessions/{source}/language`: pin the language and transcribe again.
pub async fn language_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
    axum::Json(body): axum::Json<LanguageIn>,
) -> Response {
    let language = match parse_language(body.language.as_deref()) {
        Ok(language) => language,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    let root = st.root.clone();
    match tokio::task::spawn_blocking(move || {
        let meaning = work::open_write(&root)?;
        let ingest = crate::store::open(&root)?;
        set_language(
            &meaning,
            &ingest,
            &source,
            language,
            &audiocore::instant::Stamp::now(),
        )
    })
    .await
    {
        Ok(Ok(_)) => route::ack(),
        Ok(Err(err)) => err.into_response("session language"),
        Err(err) => route::faulted("session language task", &err),
    }
}

pub async fn name_voice_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
    axum::Json(body): axum::Json<VoiceNameIn>,
) -> Response {
    let root = st.root.clone();
    // An empty name clears the label.
    let name = body
        .name
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty());
    match route::blocking("name voice", move || {
        name_voice(
            &work::open_write(&root)?,
            &source,
            &body.cluster,
            name.as_deref(),
        )
    })
    .await
    {
        Ok(_) => route::ack(),
        Err(response) => response,
    }
}

pub async fn transcript_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
) -> Response {
    let root = st.root.clone();
    route::json("session transcript", move || {
        let conn = reads::open(&root)?;
        let turns = session_turns(&conn, &source)?;
        Ok(clean_transcript(&source, &turns))
    })
    .await
}

// --- deleting a session ------------------------------------------------------

/// Proof that an upload's meaning-plane rows were deleted: the only thing
/// [`crate::store::forget_upload`] accepts, and only [`delete_session`] makes
/// one, so ingest rows can be removed only for a deleted upload, never for
/// household capture.
#[derive(Debug)]
pub struct DeletedUpload {
    source: String,
    paths: Vec<String>,
}

impl DeletedUpload {
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The audio files the deleted rows named, for the caller to unlink.
    pub fn paths(&self) -> &[String] {
        &self.paths
    }
}

/// Delete an uploaded session and everything derived from it: the one
/// irreversible operation (everything else hides, supersedes or re-derives),
/// so it is guarded to uploads by `require_upload`.
///
/// Each deleted segment is tombstoned in the same transaction, so the turns
/// pass (`turns::tombstoned_block`) does not rebuild it. `transcript_fts` is
/// contentless FTS5 with no per-row delete; a rowid whose segment is gone
/// resolves to nothing.
pub fn delete_session(
    conn: &mut Connection,
    source: &str,
    now: &Stamp,
) -> Result<DeletedUpload, SessionError> {
    require_upload(conn, source)?;
    let tx = crate::sql::write(conn)?;
    let segments: Vec<(i64, String, String)> = {
        let mut stmt = SESSION_AUDIO.prepare(&tx)?;
        let rows = stmt.query_map([source], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (_, _, start_utc) in &segments {
        REMEMBER_DELETED.execute(&tx, (source, start_utc, now))?;
    }
    for (audio_id, _, _) in &segments {
        let turn_ids: Vec<i64> = {
            let mut stmt = CLIP_TURNS.prepare(&tx)?;
            let rows = stmt.query_map([audio_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for turn_id in turn_ids {
            DROP_TURN_EMBEDDING.execute(&tx, [turn_id])?;
        }
        DROP_CORRECTIONS.execute(&tx, [audio_id])?;
        crate::turn_store::delete_for_audio(&tx, *audio_id)?;
    }
    DROP_REFINE_REQUESTS.execute(&tx, [source])?;
    DROP_AUDIO.execute(&tx, [source])?;
    DROP_SOURCE.execute(&tx, [source])?;
    tx.commit()?;
    Ok(DeletedUpload {
        source: source.to_owned(),
        paths: segments.into_iter().map(|(_, path, _)| path).collect(),
    })
}

/// Unlink a deleted session's audio and its own directory, and its ingest
/// directory only if empty: anything still there is not this delete's. Best
/// effort: the rows are already gone.
pub fn remove_files(root: &std::path::Path, source: &str, paths: &[String]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
    let own = root.join(source);
    if own.is_dir() {
        let _ = std::fs::remove_dir_all(&own);
    }
    let _ = std::fs::remove_dir(crate::store::source_dir(root, source));
}

pub async fn delete_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
) -> Response {
    let root = st.root.clone();
    let now = audiocore::instant::Stamp::now();
    let done = tokio::task::spawn_blocking(move || -> Result<(), SessionError> {
        let deleted = {
            let mut conn = work::open_write(&root)?;
            delete_session(&mut conn, &source, &now)?
        };
        // The ingest plane holds the session's words in its jobs.
        let mut ingest = crate::store::open(&root)?;
        crate::store::forget_upload(&mut ingest, &deleted)?;
        // Files only after the commits, in case a delete rolls back.
        remove_files(&root, &source, deleted.paths());
        Ok(())
    });
    match done.await {
        Ok(Ok(())) => route::ack(),
        Ok(Err(err)) => err.into_response("session delete"),
        Err(err) => route::faulted("session delete task", &err),
    }
}
