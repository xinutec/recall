//! The uploaded-sessions surface.
//!
//! A *session* is one discrete recording, such as a meeting, as opposed to the
//! continuous capture. These routes find, name and read one back; the upload
//! itself is `crate::upload`.
//!
//! Every mutating route is guarded to upload sources: renaming or re-diarizing
//! the continuous archive would be wrong, deleting it unrecoverable. The guard
//! answers 404 for a source that does not exist and 400 for one that is not an
//! upload.

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

/// A source that arrived as a file rather than a live microphone.
const UPLOAD_KIND: &str = "upload";

/// A diarization tag, as opposed to a name a person gave. Never shown as a
/// speaker: `SPEAKER_00` is an answer about voices, not about people.
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

/// Every uploaded session, newest first.
///
/// Only human-confirmed names are listed as speakers; a turn without one counts
/// as "unknown". Voiceprint guesses have no trustworthy threshold (see
/// [`crate::identify::match_one`]).
pub fn sessions(conn: &Connection) -> rusqlite::Result<SessionsOut> {
    let mut stmt = SESSIONS.prepare(conn)?;
    let rows = stmt.query_map([UPLOAD_KIND], |r| {
        let names: Option<String> = r.get(5)?;
        // Sorted, not first-seen: this is a list to scan, and GROUP_CONCAT's
        // order is not defined.
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

/// Why a session could not be acted on.
#[derive(Debug)]
pub enum SessionError {
    /// No source with that id.
    Missing,
    /// It exists, but it is the continuous archive rather than an upload.
    NotAnUpload,
    /// It has no audio, so there is no span to work over.
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

/// Refuse anything that is not an uploaded meeting.
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

/// Re-derive who said what across a whole session: every finished
/// `diarize-segment` job of its clips goes back to `queued` and the diarized
/// pass's ledger rows for them are cleared, so the voices runner diarizes them
/// again and the pass decides afresh against the turns standing now.
///
/// Returns how many clips were re-queued. Zero is not an error: a session whose
/// diarization has not finished yet has nothing to redo.
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

/// The languages a session can be pinned to: the household's two. The model's
/// own guess picks a third language for Dutch often enough (Italian for a Dutch
/// conversation, 2026-10-03) that anything else is more likely a mistake.
pub const LANGUAGES: &[&str] = &["nl", "en"];

/// A requested language: absent or empty is the model's guess, one of
/// [`LANGUAGES`] pins it, anything else is refused.
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

/// Pin `source`'s language (or unpin it) and transcribe every clip again with
/// it, through [`crate::retranscribe`]: the old lines are set aside, not
/// deleted, and a person's own lines are kept.
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

/// Store `source`'s language without transcribing anything again: for a
/// session being created, which has no transcription yet.
pub fn pin_language(
    conn: &Connection,
    source: &str,
    language: Option<&str>,
) -> rusqlite::Result<()> {
    SET_LANGUAGE.execute(conn, (source, language))?;
    Ok(())
}

/// The language `source` is pinned to, if any.
pub fn language_of(conn: &Connection, source: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(LANGUAGE_OF
        .query_row(conn, [source], |r| r.get(0))
        .optional()?
        .flatten())
}

/// Give a leased transcription job its session's pinned language, if any;
/// attached at lease time so a change applies to the next transcription.
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

/// Name a diarization voice across a whole session, or clear it with `None`.
///
/// No `hidden_reason` filter, unlike the read queries: hiding is a display
/// state, and a turn unhidden later must come back correctly named.
///
/// Not display-only: the voiceprint backfill selects on `speaker_label`, so
/// naming a voice here enrols it.
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

/// One turn as the export reads it. Public so the merge rule can be tested by
/// constructing turns rather than a schema.
pub struct ExportTurn {
    pub start_utc: String,
    pub text: String,
    pub speaker_label: Option<String>,
    pub speaker_cluster: Option<String>,
}

/// A confirmed name wins; else the diarization voice, so distinct unnamed
/// speakers stay distinguishable; "unknown" only when there is neither.
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

/// The finalised transcript: consecutive same-speaker turns merged into one
/// bubble, current state only, deterministic so an unchanged re-export is
/// byte-identical.
///
/// Bubble times are in the machine's local zone, for a person reading a
/// document. The pod runs in UTC.
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

/// A stored UTC instant in the machine's local zone, or unchanged if it will
/// not parse — a transcript is worth exporting with an odd timestamp, and is
/// not worth failing over one.
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
    // An empty name clears the label rather than storing "", which would read as
    // a speaker called nothing.
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

/// Delete an uploaded session and everything derived from it, returning the audio
/// file paths for the caller to unlink.
///
/// ⚠ The one irreversible operation in this product: everything else hides,
/// supersedes or re-derives. It must stay guarded to upload sources by
/// `require_upload`; the continuous capture is append-only.
///
/// ⚠ Every deleted segment is tombstoned in the same transaction, so the turns
/// pass (`turns::tombstoned_block`) does not rebuild it. A record, never an
/// order: nothing serves it to a recorder.
///
/// `transcript_fts` is deliberately not cleaned: it is contentless FTS5 with no
/// per-row delete, and a rowid whose segment is gone resolves to nothing.
/// Proof that an upload's meaning-plane rows were deleted: the only thing
/// [`crate::store::forget_upload`] accepts, and only [`delete_session`] makes
/// one. So the ingest rows of a source can be removed only for a source just
/// checked to be an upload and deleted, never for household capture.
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

/// Unlink a deleted session's audio, its own directory, and its ingest
/// directory once that is empty. Best effort: the rows are already gone.
///
/// The ingest directory is removed only when empty, never recursively: it is
/// the ingest plane, and anything still in it is not this delete's to destroy.
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
        // The words too: the ingest plane holds the session's transcription
        // in its jobs, and a delete that leaves them has not deleted anything
        // a reader cares about.
        let mut ingest = crate::store::open(&root)?;
        crate::store::forget_upload(&mut ingest, &deleted)?;
        // Files only after the commits: unlinking first would destroy audio a
        // rolled-back delete still points at.
        remove_files(&root, &source, deleted.paths());
        Ok(())
    });
    match done.await {
        Ok(Ok(())) => route::ack(),
        Ok(Err(err)) => err.into_response("session delete"),
        Err(err) => route::faulted("session delete task", &err),
    }
}
