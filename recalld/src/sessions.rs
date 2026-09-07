//! The uploaded-sessions surface, ported from `recall.api_sessions`.
//!
//! A *session* is one discrete recording — a hospital appointment, a meeting —
//! as opposed to the household's continuous capture. It is use case 2 of the
//! two this product serves, so these routes are how a meeting is found, named
//! and read back.
//!
//! ⚠ **Every mutating route is guarded to UPLOAD sources.** The continuous
//! capture archive is append-only and must never be reachable through a path
//! meant for meetings — renaming or re-diarizing it here would be wrong, and
//! deleting it would be unrecoverable. The guard answers 404 for a source that
//! does not exist and 400 for one that exists but is not an upload, so a caller
//! can tell "no such meeting" from "that is the household archive".
//!
//! ⚠ **Two routes are deliberately NOT here**: the multipart upload and the
//! delete. Delete removes turns, audio rows AND files from disk, which is the
//! one irreversible operation in the product; it stays with the Python that has
//! been running it. `crate::app` forwards both to the upstream by method, which
//! is why porting half a path is safe at all.

use crate::{reads, route, work};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A source that arrived as a file rather than a live microphone.
const UPLOAD_KIND: &str = "upload";

/// A diarization tag, as opposed to a name a person gave. Never shown as a
/// speaker: `SPEAKER_00` is an answer about voices, not about people.
const CLUSTER_PREFIX: &str = "SPEAKER";

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionOut {
    pub id: String,
    pub title: String,
    pub start: String,
    pub end: String,
    pub turn_count: i64,
    pub speakers: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct SessionsOut {
    pub items: Vec<SessionOut>,
}

/// Every uploaded session, newest first.
///
/// ⚠ **Only human-confirmed names are listed as speakers.** Voiceprint guesses
/// are deliberately excluded: on out-of-domain audio a visitor can score 0.95
/// against an enrolled household member, so no threshold separates true from
/// false and a name chip here would assert an attribution nobody made. A turn
/// with no confirmed name counts as "unknown" instead.
pub fn sessions(conn: &Connection) -> rusqlite::Result<SessionsOut> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.name, MIN(a.start_utc), MAX(a.end_utc), COUNT(t.id), \
                GROUP_CONCAT(DISTINCT CASE \
                    WHEN t.id IS NULL THEN NULL \
                    WHEN t.speaker_label IS NOT NULL \
                         AND t.speaker_label NOT LIKE 'SPEAKER_%' \
                         THEN t.speaker_label \
                    ELSE 'unknown' \
                END) \
         FROM sources s \
         JOIN audio_segments a ON a.source_id = s.id \
         LEFT JOIN transcript_segments t \
                ON t.audio_segment_id = a.id \
               AND t.superseded_by IS NULL AND t.hidden_reason IS NULL \
         WHERE s.kind = ?1 \
         GROUP BY s.id \
         ORDER BY MIN(a.start_utc) DESC",
    )?;
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
    /// It exists, but it is the household archive rather than a meeting.
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
    let kind: Option<String> = conn
        .query_row("SELECT kind FROM sources WHERE id = ?1", [source], |r| {
            r.get(0)
        })
        .ok();
    match kind.as_deref() {
        None => Err(SessionError::Missing),
        Some(UPLOAD_KIND) => Ok(()),
        Some(_) => Err(SessionError::NotAnUpload),
    }
}

pub fn rename(conn: &Connection, source: &str, title: &str) -> Result<(), SessionError> {
    require_upload(conn, source)?;
    conn.execute(
        "UPDATE sources SET name = ?1 WHERE id = ?2",
        (title, source),
    )?;
    Ok(())
}

/// Queue a whole-session re-derivation of who said what.
///
/// ⚠ Queued, never run inline: diarization is the expensive pass and running it
/// on a request thread would starve live capture. The idle-gated daemon picks it
/// up, which is the same contract `work::add_refine_request` serves.
pub fn rediarize(conn: &Connection, source: &str, now: &str) -> Result<(), SessionError> {
    require_upload(conn, source)?;
    let span: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT MIN(start_utc), MAX(end_utc) FROM audio_segments WHERE source_id = ?1",
            [source],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let Some((Some(start), Some(end))) = span else {
        return Err(SessionError::NoAudio);
    };
    work::add_refine_request(conn, source, &start, &end, now)?;
    Ok(())
}

/// Name a diarization voice across a whole session, or clear it with `None`.
///
/// ⚠ **No `hidden_reason` filter, unlike every read query.** Hiding is a display
/// state; who spoke is a fact about the turn. A hidden turn that is later
/// unhidden must come back correctly named, so the write covers it too.
///
/// This is not display-only: `speaker_label` is the work-list the voiceprint
/// backfill selects on, so naming a meeting's clinician enrols them in the
/// matching pool like any household voice.
pub fn name_voice(
    conn: &Connection,
    source: &str,
    cluster: &str,
    name: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE transcript_segments SET speaker_label = ?1 WHERE id IN ( \
             SELECT ts.id FROM transcript_segments ts \
             JOIN audio_segments a ON a.id = ts.audio_segment_id \
             WHERE a.source_id = ?2 AND ts.speaker_cluster = ?3 \
               AND ts.superseded_by IS NULL)",
        (name, source, cluster),
    )
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
    let mut stmt = conn.prepare(
        "SELECT t.start_utc, t.text, t.speaker_label, t.speaker_cluster \
         FROM transcript_segments t \
         JOIN audio_segments a ON a.id = t.audio_segment_id \
         WHERE a.source_id = ?1 AND t.superseded_by IS NULL \
           AND t.hidden_reason IS NULL \
         ORDER BY t.start_utc",
    )?;
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
/// ⚠ Bubble times are LOCAL, not UTC — this is written for a person to read in
/// a document, and `astimezone()` with no argument is what the Python does. Both
/// containers run UTC, so the pod's output is unchanged by the port; a run on a
/// machine in another zone differs in both implementations alike.
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

#[derive(Deserialize)]
pub struct RenameIn {
    title: String,
}

#[derive(Deserialize)]
pub struct VoiceNameIn {
    cluster: String,
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
    let now = chrono::Utc::now().to_rfc3339();
    match tokio::task::spawn_blocking(move || rediarize(&work::open_write(&root)?, &source, &now))
        .await
    {
        Ok(Ok(())) => route::ack(),
        Ok(Err(err)) => err.into_response("session rediarize"),
        Err(err) => route::faulted("session rediarize task", &err),
    }
}

pub async fn name_voice_route(
    State(st): State<Arc<reads::State>>,
    Path(source): Path<String>,
    axum::Json(body): axum::Json<VoiceNameIn>,
) -> Response {
    let root = st.root.clone();
    // An empty name CLEARS the label rather than storing "", which would read as
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
