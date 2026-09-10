//! Uploading a discrete recording — a hospital appointment, a meeting — as a new
//! session. Ported from `recall.api_sessions.create_session`.
//!
//! This is use case 2's front door: a file arrives, becomes a source, and the
//! worker transcribes it while the idle-gated daemon diarizes it. The session
//! appears in the list AT ONCE with zero turns, because an upload that showed
//! nothing until transcription finished would look like it had failed.
//!
//! ⚠ **The container is kept, not forced to WAV.** ffprobe validates what is
//! actually inside; the suffix only gates what is worth trying.

use crate::{instant, pyjson};
use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};
use chrono_tz::Europe::London;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Containers a conversation recording might arrive in — phone voice memos are
/// m4a, most recorders export mp3.
const AUDIO_SUFFIXES: &[&str] = &[
    ".mp3", ".m4a", ".mp4", ".wav", ".flac", ".aac", ".ogg", ".opus", ".webm",
];

/// A source that arrived as a file rather than a live microphone.
const UPLOAD_KIND: &str = "upload";

const S16_BYTES_PER_SAMPLE: usize = 2;

#[derive(Debug)]
pub enum UploadError {
    /// The suffix is not one we would even try to decode.
    UnsupportedType(String),
    /// ffprobe or ffmpeg could not read it, so it is not audio we can use.
    Unreadable,
    /// The client's `start` is not an ISO-8601 instant.
    BadStart,
    Io(std::io::Error),
    Db(rusqlite::Error),
}

impl From<std::io::Error> for UploadError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<rusqlite::Error> for UploadError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// What an audio file holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Media {
    pub duration_s: f64,
    pub sample_rate: i64,
    pub channels: i64,
}

/// Sample rate and channels from the stream header, which is reliable.
///
/// ⚠ The DURATION is not taken from the header. Segment-muxer output carries
/// none, so it is measured by decoding — see [`decode_duration`].
pub fn probe(path: &Path) -> Result<Media, UploadError> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-of",
            "json",
            "-show_entries",
            "stream=sample_rate,channels",
        ])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(UploadError::Unreadable);
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|_| UploadError::Unreadable)?;
    let stream = parsed
        .get("streams")
        .and_then(|s| s.get(0))
        .ok_or(UploadError::Unreadable)?;
    // ffprobe reports sample_rate as a STRING and channels as a number.
    let sample_rate: i64 = stream
        .get("sample_rate")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse().ok())
        .ok_or(UploadError::Unreadable)?;
    let channels: i64 = stream
        .get("channels")
        .and_then(serde_json::Value::as_i64)
        .ok_or(UploadError::Unreadable)?;
    if sample_rate <= 0 || channels <= 0 {
        return Err(UploadError::Unreadable);
    }
    Ok(Media {
        duration_s: decode_duration(path, sample_rate, channels)?,
        sample_rate,
        channels,
    })
}

/// Exact duration by decoding to raw PCM and counting bytes.
///
/// ⚠ Header-independent on purpose, and exact at any length — including
/// sub-second trailing segments, where ffmpeg's human-readable progress reports
/// `time=N/A` and a header-derived answer is simply absent.
fn decode_duration(path: &Path, sample_rate: i64, channels: i64) -> Result<f64, UploadError> {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args([
            "-f",
            "s16le",
            "-ac",
            &channels.to_string(),
            "-ar",
            &sample_rate.to_string(),
            "-",
        ])
        .output()?;
    if !out.status.success() {
        return Err(UploadError::Unreadable);
    }
    let frames = out.stdout.len() / (S16_BYTES_PER_SAMPLE * channels as usize);
    Ok(frames as f64 / sample_rate as f64)
}

/// The suffix, lowercased, including the dot — or empty when there is none.
pub fn suffix_of(filename: &str) -> String {
    Path::new(filename)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

/// A meeting's id and default title, from its LOCAL start.
///
/// ⚠ Local is Europe/London, not the host's zone. The pod runs UTC, so deriving
/// this from the container clock would shift the id by an hour every summer and
/// mint a different id for the same recording.
pub fn meeting_id(started: DateTime<Utc>) -> (String, String) {
    let local = started.with_timezone(&London);
    // ⚠ **The autumn clock change makes a local time ambiguous, and the id is
    // local** (#1476). On the night the clocks go back, 01:00-02:00 happens
    // TWICE, so 00:30Z and 01:30Z both read as 01:30 and derived ONE id. The
    // second upload then registered the id the first already held — an UPSERT,
    // not a refusal — its segment landed under the same source because the start
    // times differ, and its audio was written into the first meeting's
    // directory. Two recordings became one session and nothing errored.
    //
    // Marking the REPEAT rather than rebasing to UTC is the fix that costs
    // nothing: the id stays the local time a person reads, every meeting that
    // already exists keeps its spelling, and only the second pass through a
    // repeated hour gains a marker. Deriving it from the instant rather than
    // from the database also keeps it idempotent — the same recording uploaded
    // twice still lands on one id, where a collision check would mint a second.
    let marker = repeated_hour_marker(started, &local);
    (
        format!(
            "meeting-{:04}{:02}{:02}-{:02}{:02}{}",
            local.year(),
            local.month(),
            local.day(),
            local.hour(),
            local.minute(),
            marker
                .as_ref()
                .map_or(String::new(), |z| format!("-{}", z.to_lowercase()))
        ),
        format!(
            "Meeting {:04}-{:02}-{:02} {:02}:{:02}{}",
            local.year(),
            local.month(),
            local.day(),
            local.hour(),
            local.minute(),
            marker.as_ref().map_or(String::new(), |z| format!(" {z}"))
        ),
    )
}

/// The zone abbreviation, but ONLY for the second pass through a repeated local
/// hour — otherwise `None`.
///
/// ⚠ The control that matters is an ordinary winter meeting: it is in GMT too,
/// and marking every GMT meeting would rename everything from November to March.
/// What distinguishes the repeat is that its local time maps back to TWO
/// instants, and this one is the later of them.
fn repeated_hour_marker(started: DateTime<Utc>, local: &DateTime<chrono_tz::Tz>) -> Option<String> {
    use chrono::offset::LocalResult;
    match London.from_local_datetime(&local.naive_local()) {
        LocalResult::Ambiguous(_, latest) if latest.with_timezone(&Utc) == started => {
            Some(local.format("%Z").to_string())
        }
        _ => None,
    }
}

/// Where the uploaded file lands: its own directory under the data root.
pub fn stored_path(root: &Path, source: &str, started: DateTime<Utc>, suffix: &str) -> PathBuf {
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}",
        started.year(),
        started.month(),
        started.day(),
        started.hour(),
        started.minute(),
        started.second()
    );
    root.join(source).join(format!("{source}-{stamp}{suffix}"))
}

/// Register the source and its one audio segment.
///
/// ⚠ **REGISTER, not add.** The worker scans the data root continuously and may
/// already have claimed this directory with a DISCOVERED kind and a placeholder
/// name. An `INSERT OR IGNORE` would leave that guess standing and the session
/// would never appear in the list — because the list selects on `kind='upload'`.
/// The name is preserved unless it is still the placeholder: a title the user
/// chose is theirs.
pub fn register(
    conn: &Connection,
    source: &str,
    name: &str,
    path: &Path,
    started: DateTime<Utc>,
    media: Media,
) -> Result<(), UploadError> {
    conn.execute(
        "INSERT INTO sources (id, name, kind, port) VALUES (?1, ?2, ?3, NULL) \
         ON CONFLICT(id) DO UPDATE SET \
             kind = excluded.kind, \
             port = excluded.port, \
             name = CASE WHEN sources.name = sources.id \
                         THEN excluded.name ELSE sources.name END",
        (source, name, UPLOAD_KIND),
    )?;
    let end = started + chrono::Duration::microseconds((media.duration_s * 1e6).round() as i64);
    conn.execute(
        "INSERT OR IGNORE INTO audio_segments \
             (source_id, path, start_utc, end_utc, sample_rate, channels) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            source,
            path.to_string_lossy(),
            iso(started),
            iso(end),
            media.sample_rate,
            media.channels,
        ],
    )?;
    Ok(())
}

fn iso(at: DateTime<Utc>) -> String {
    instant::python_isoformat(&at.to_rfc3339()).unwrap_or_else(|| at.to_rfc3339())
}

/// Whether this suffix is worth handing to ffprobe.
pub fn is_supported(suffix: &str) -> bool {
    AUDIO_SUFFIXES.contains(&suffix)
}

/// The client's `start`, or now.
pub fn started_at(raw: &str) -> Result<DateTime<Utc>, UploadError> {
    if raw.is_empty() {
        return Ok(Utc::now());
    }
    let normalised = instant::python_isoformat(raw).ok_or(UploadError::BadStart)?;
    DateTime::parse_from_rfc3339(&normalised)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| UploadError::BadStart)
}

/// The session as the list renders it, for the response.
pub fn created_json(source: &str, title: &str, started: DateTime<Utc>, duration_s: f64) -> String {
    let end = started + chrono::Duration::microseconds((duration_s * 1e6).round() as i64);
    pyjson::dump(&serde_json::json!({
        "id": source,
        "title": title,
        "start": iso(started),
        "end": iso(end),
        "turnCount": 0,
        "speakers": [],
    }))
}

/// London, exported so a test can state the zone it is asserting about.
pub fn london_offset_hours(at: DateTime<Utc>) -> i32 {
    use chrono::Offset;
    London
        .from_utc_datetime(&at.naive_utc())
        .offset()
        .fix()
        .local_minus_utc()
        / 3600
}

// --- HTTP -------------------------------------------------------------------

use crate::{reads, route, work};
use axum::extract::{Multipart, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// The three parts the form carries. `title` and `start` are optional.
struct Form {
    filename: String,
    bytes: Vec<u8>,
    title: String,
    start: String,
}

async fn read_form(mut parts: Multipart) -> Result<Form, Response> {
    let (mut filename, mut bytes, mut title, mut start) =
        (None, None, String::new(), String::new());
    loop {
        let field = match parts.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                return Err(
                    (StatusCode::BAD_REQUEST, format!("malformed upload: {err}")).into_response(),
                );
            }
        };
        match field.name().unwrap_or_default().to_owned().as_str() {
            "audio" => {
                filename = Some(field.file_name().unwrap_or_default().to_owned());
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|err| {
                            (
                                StatusCode::BAD_REQUEST,
                                format!("could not read audio: {err}"),
                            )
                                .into_response()
                        })?
                        .to_vec(),
                );
            }
            "title" => title = field.text().await.unwrap_or_default(),
            "start" => start = field.text().await.unwrap_or_default(),
            _ => {}
        }
    }
    let (Some(filename), Some(bytes)) = (filename, bytes) else {
        return Err((StatusCode::BAD_REQUEST, "an audio file is required").into_response());
    };
    Ok(Form {
        filename,
        bytes,
        title,
        start,
    })
}

pub async fn create_session_route(
    State(st): State<Arc<reads::State>>,
    parts: Multipart,
) -> Response {
    let form = match read_form(parts).await {
        Ok(form) => form,
        Err(response) => return response,
    };
    let Ok(started) = started_at(form.start.trim()) else {
        return (StatusCode::BAD_REQUEST, "a valid ISO 8601 time is required").into_response();
    };
    let suffix = suffix_of(&form.filename);
    if !is_supported(&suffix) {
        return (
            StatusCode::BAD_REQUEST,
            format!("unsupported audio type {suffix:?}"),
        )
            .into_response();
    }
    let root = st.root.clone();
    let title = form.title.trim().to_owned();

    let done = tokio::task::spawn_blocking(move || -> Result<String, UploadError> {
        let (source, default_title) = meeting_id(started);
        let name = if title.is_empty() {
            default_title
        } else {
            title
        };
        let path = stored_path(&root, &source, started, &suffix);
        std::fs::create_dir_all(path.parent().expect("a file has a parent"))?;
        std::fs::write(&path, &form.bytes)?;
        // ⚠ Probed AFTER writing and removed if it will not read: a file we
        // cannot decode must not be left behind for the worker to find and
        // register as a source of its own.
        let media = match probe(&path) {
            Ok(media) => media,
            Err(err) => {
                let _ = std::fs::remove_file(&path);
                return Err(err);
            }
        };
        register(
            &work::open_write(&root)?,
            &source,
            &name,
            &path,
            started,
            media,
        )?;
        Ok(created_json(&source, &name, started, media.duration_s))
    });

    match done.await {
        Ok(Ok(body)) => ([(header::CONTENT_TYPE, "application/json")], body).into_response(),
        Ok(Err(UploadError::Unreadable)) => {
            (StatusCode::BAD_REQUEST, "could not read audio").into_response()
        }
        Ok(Err(UploadError::UnsupportedType(suffix))) => (
            StatusCode::BAD_REQUEST,
            format!("unsupported audio type {suffix:?}"),
        )
            .into_response(),
        Ok(Err(UploadError::BadStart)) => {
            (StatusCode::BAD_REQUEST, "a valid ISO 8601 time is required").into_response()
        }
        Ok(Err(UploadError::Io(err))) => route::faulted("upload write", &err),
        Ok(Err(UploadError::Db(err))) => route::faulted("upload register", &err),
        Err(err) => route::faulted("upload task", &err),
    }
}
