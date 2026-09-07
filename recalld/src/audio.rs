//! Playback clips (stage F1), ported from `recall.api_audio`.
//!
//! Two routes, both read-only: one turn's audio, and one continuous span across a
//! run of same-speaker turns. Like [`crate::reads`], this opens `recall.sqlite`
//! READ-ONLY — recalld does not own the meaning plane.
//!
//! ⚠ **The clip is shaped, the recording is not.** Every clip is sliced out with
//! ffmpeg and then peak-normalised with sox, so a quiet turn is audible without
//! reaching for the volume. The archived audio is never touched; only the
//! transient clip is. Both binaries are the ones the segmenters already use.
//!
//! ⚠ **Those two binaries are a RUNTIME dependency of recalld now, and a missing
//! one fails at play time rather than at boot.** The fleet image installs
//! `ffmpeg sox flac` for exactly this reason and says so: it once shipped with
//! ffmpeg alone, and every audio request on the fleet died inside loudness
//! normalisation while the transcripts served perfectly — a fault that hides
//! until somebody presses play. recalld runs from that same image, so it
//! inherits both the dependency and the failure mode.
//!
//! ⚠ **The padding rule is the whole reason this is not a generic byte-range
//! server.** A Whisper turn is a phrase — slicing exactly to it yields a
//! one-second fragment with no lead-in, which is useless for recall. So a rough
//! turn gets a wide context window, while a *precise* cutout (a diarized turn, or
//! one carrying word timings) gets a tight one, because widening that would pull
//! in the neighbouring speaker — the exact confusion diarization just resolved.

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Lead-in/-out for a rough whole-phrase turn.
const PAD_S: f64 = 1.5;
/// Minimum length for a rough turn, so even a one-word turn is listenable.
const MIN_S: f64 = 5.0;
/// Safety pad for a precise cutout — the diarization boundary is approximate, so
/// onsets and offsets would otherwise clip.
const TIGHT_PAD_S: f64 = 0.2;

/// Widen a `[start, end]` phrase span (seconds within its audio file) for
/// playback: `pad` on each side, then expanded symmetrically to at least
/// `minimum`. Start clamps at 0; the end may run past the file, where ffmpeg
/// simply stops at EOF.
#[must_use]
pub fn clip_window(phrase_start: f64, phrase_end: f64, pad: f64, minimum: f64) -> (f64, f64) {
    let mut start = phrase_start - pad;
    let mut end = phrase_end + pad;
    if end - start < minimum {
        let mid = f64::midpoint(phrase_start, phrase_end);
        start = mid - minimum / 2.0;
        end = mid + minimum / 2.0;
    }
    (start.max(0.0), end)
}

/// One turn's placement: which file holds it, and where inside that file.
#[derive(Debug, PartialEq)]
pub struct Placement {
    pub path: PathBuf,
    pub audio_segment_id: i64,
    /// Seconds from the start of the audio file.
    pub start_s: f64,
    pub end_s: f64,
    /// A precise cutout, played tight — see the module note.
    pub precise: bool,
}

/// `asr_model` of the provisional live pass.
const LIVE_MODEL: &str = "live";
/// `asr_model` of a turn a human corrected.
const HUMAN_MODEL: &str = "human";
/// `provenance` prefix written by the diarized refine pass.
const DIARIZED_MARKER: &str = "diarized";

/// Where a turn's audio lives, or `None` if it has none.
///
/// Times are computed as an offset from the audio segment's own start, exactly
/// as the Python does — the turn's absolute timestamps mean nothing to ffmpeg.
///
/// ⚠ The offset comes from `SQLite`'s `julianday`, which is a float day count, so
/// it carries roughly 10µs of error at these magnitudes. That is three orders of
/// magnitude below the millisecond precision `-ss` is formatted to, so it can
/// never move a frame — but it does mean these seconds are not bit-identical to
/// the Python's datetime subtraction, and a test comparing them needs a
/// tolerance rather than equality.
pub fn placement(conn: &Connection, transcript_id: i64) -> rusqlite::Result<Option<Placement>> {
    let mut stmt = conn.prepare(
        "SELECT a.path, a.id, \
                (julianday(t.start_utc) - julianday(a.start_utc)) * 86400.0, \
                (julianday(t.end_utc)   - julianday(a.start_utc)) * 86400.0, \
                t.asr_model, t.provenance, t.word_timings \
         FROM transcript_segments t \
         JOIN audio_segments a ON a.id = t.audio_segment_id \
         WHERE t.id = ?1",
    )?;
    let mut rows = stmt.query([transcript_id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let asr_model: Option<String> = row.get(4)?;
    let provenance: Option<String> = row.get(5)?;
    let word_timings: Option<String> = row.get(6)?;
    let diarized = provenance
        .as_deref()
        .unwrap_or("")
        .starts_with(DIARIZED_MARKER)
        && asr_model.as_deref() != Some(HUMAN_MODEL)
        && asr_model.as_deref() != Some(LIVE_MODEL);
    Ok(Some(Placement {
        path: PathBuf::from(row.get::<_, String>(0)?),
        audio_segment_id: row.get(1)?,
        start_s: row.get(2)?,
        end_s: row.get(3)?,
        precise: diarized || word_timings.is_some(),
    }))
}

/// Slice `[start, end]` out of `src` and peak-normalise it, returning WAV bytes.
///
/// ⚠ Two processes, not one: ffmpeg cuts, sox normalises. Keeping sox's `norm -1`
/// rather than reaching for an ffmpeg filter keeps playback loudness identical to
/// what the Python served, which is a thing a person would notice change.
pub fn render(src: &Path, start: f64, end: f64) -> std::io::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let cut = dir.path().join("clip.wav");
    let norm = dir.path().join("clip-norm.wav");
    let sliced = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(["-ss", &format!("{start:.3}"), "-to", &format!("{end:.3}")])
        .arg(&cut)
        .status()?;
    if !sliced.success() {
        return Err(std::io::Error::other("ffmpeg slice failed"));
    }
    let normalised = Command::new("sox")
        .arg(&cut)
        .arg(&norm)
        .args(["norm", "-1"])
        .status()?;
    if !normalised.success() {
        return Err(std::io::Error::other("sox norm failed"));
    }
    std::fs::read(&norm)
}

/// The window to play for one turn — the padding rule, applied.
#[must_use]
pub fn window_for(p: &Placement) -> (f64, f64) {
    if p.precise {
        clip_window(p.start_s, p.end_s, TIGHT_PAD_S, 0.0)
    } else {
        clip_window(p.start_s, p.end_s, PAD_S, MIN_S)
    }
}

/// The window for a joined bubble: the first turn's start to the last turn's end,
/// always tight — a bubble is a run of turns already snapped to one speaker.
///
/// `Err(SpanError::CrossesRecordings)` when the two turns are not in the same
/// file; the UI falls back to per-turn playback rather than being handed a clip
/// spliced across a gap.
pub fn span_window(first: &Placement, last: &Placement) -> Result<(f64, f64), SpanError> {
    if first.audio_segment_id != last.audio_segment_id {
        return Err(SpanError::CrossesRecordings);
    }
    Ok(clip_window(first.start_s, last.end_s, TIGHT_PAD_S, 0.0))
}

/// Why a span could not be rendered as one clip.
#[derive(Debug, PartialEq, Eq)]
pub enum SpanError {
    /// The two turns live in different recordings.
    CrossesRecordings,
}

// --- HTTP ------------------------------------------------------------------

use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct SpanQuery {
    from_id: i64,
    to_id: i64,
}

/// Why a clip could not be produced.
///
/// ⚠ **Errors as data, not as pre-rendered responses.** The picker used to
/// return a `Box<Response>`, which meant a database query decided an HTTP status
/// from inside a `SQLite` closure, and every caller had to box a fat value to
/// say "the query failed". The status belongs at the edge; this is what the
/// picker actually knows.
#[derive(Debug)]
pub enum ClipError {
    /// The lookup failed. Nothing is known about whether audio exists.
    Db(rusqlite::Error),
    /// Both turns exist, but they are not one stretch of one recording.
    CrossesRecordings,
}

impl From<rusqlite::Error> for ClipError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Db(err)
    }
}

/// Render `[start, end)` of one recording as a WAV response.
///
/// ⚠ Private, and reached only through [`render_blocking`], because it shells
/// out to ffmpeg and sox. A caller on the request thread stalls every other
/// browsing request — and the recorders' ingest, which shares this runtime —
/// for the length of a clip. Making that mistake possible cost nothing to
/// prevent: nobody outside needs to render without first picking.
fn clip(path: &Path, start: f64, end: f64) -> Response {
    match render(path, start, end) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "audio/wav")], bytes).into_response(),
        Err(err) => crate::route::faulted("clip render", &err),
    }
}

fn no_audio() -> Response {
    (StatusCode::NOT_FOUND, "no audio").into_response()
}

pub type Picked = Result<Option<(PathBuf, f64, f64)>, ClipError>;

/// Open the read-only connection, pick a window, render it — the whole of what
/// an audio route does, and the only place [`clip`] may be called from.
///
/// ⚠ Must run on the blocking pool; see [`clip`].
pub fn render_blocking(root: &Path, pick: impl FnOnce(&Connection) -> Picked) -> Response {
    let conn = match crate::reads::open(root) {
        Ok(conn) => conn,
        Err(err) => return crate::route::faulted("audio open", &err),
    };
    match pick(&conn) {
        Err(ClipError::Db(err)) => crate::route::faulted("audio query", &err),
        // 400, not 404: both turns exist, they just are not one clip. The UI
        // reads this as "fall back to per-turn play", where a 404 would read as
        // "this bubble has no audio at all".
        Err(ClipError::CrossesRecordings) => {
            (StatusCode::BAD_REQUEST, "span crosses recordings").into_response()
        }
        Ok(None) => no_audio(),
        Ok(Some((path, start, end))) => clip(&path, start, end),
    }
}

pub async fn audio_route(
    State(st): State<Arc<crate::reads::State>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let root = st.root.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        render_blocking(&root, |conn| {
            Ok(placement(conn, id)?.map(|p| {
                let (start, end) = window_for(&p);
                (p.path.clone(), start, end)
            }))
        })
    });
    match rendered.await {
        Ok(response) => response,
        Err(err) => crate::route::faulted("audio task", &err),
    }
}

pub async fn audio_span_route(
    State(st): State<Arc<crate::reads::State>>,
    Query(q): Query<SpanQuery>,
) -> Response {
    let root = st.root.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        render_blocking(&root, |conn| {
            let (Some(first), Some(last)) =
                (placement(conn, q.from_id)?, placement(conn, q.to_id)?)
            else {
                return Ok(None);
            };
            match span_window(&first, &last) {
                Err(SpanError::CrossesRecordings) => Err(ClipError::CrossesRecordings),
                Ok((start, end)) => Ok(Some((first.path.clone(), start, end))),
            }
        })
    });
    match rendered.await {
        Ok(response) => response,
        Err(err) => crate::route::faulted("audio span task", &err),
    }
}
