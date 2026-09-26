//! Playback clips.
//!
//! Two routes, both read-only: one turn's audio, and one continuous span across a
//! run of same-speaker turns. Like [`crate::reads`], this opens `recall.sqlite`
//! read-only: recalld does not own the meaning plane.
//!
//! Each clip is sliced with ffmpeg and peak-normalised with sox, so a quiet turn
//! is audible; the archived audio is never touched.
//!
//! ⚠ ffmpeg, sox and (for `enhance=true`) `deep-filter` are runtime
//! dependencies. A missing one fails only when somebody presses play, while
//! transcripts still serve; the Dockerfile installs all three.
//!
//! Padding: a rough turn is a whole phrase, and sliced exactly it has no
//! lead-in, so it gets a wide window. A precise cutout (diarized, or carrying
//! word timings) gets a tight one, because widening it would pull in the
//! neighbouring speaker.

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

/// Where a turn's audio lives, or `None` if it has none.
///
/// Times are offsets from the audio segment's own start: ffmpeg knows nothing
/// of absolute timestamps.
///
/// The offset comes from `SQLite`'s float `julianday`, so it carries roughly
/// 10 µs of error, far below the millisecond precision `-ss` is formatted to.
/// Tests comparing these seconds need a tolerance, not equality.
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
    // Boundaries a diarized pass cut, not the tier: a turn named in place keeps
    // the transcription's looser ones.
    let aligned = provenance
        .and_then(|raw| raw.parse::<crate::turn_store::Provenance>().ok())
        .is_some_and(|p| p.is_diarized());
    let diarized = aligned
        && !matches!(
            asr_model.as_deref(),
            Some(crate::turn_store::HUMAN_MODEL | crate::turn_store::LIVE_MODEL)
        );
    Ok(Some(Placement {
        path: PathBuf::from(row.get::<_, String>(0)?),
        audio_segment_id: row.get(1)?,
        start_s: row.get(2)?,
        end_s: row.get(3)?,
        precise: diarized || word_timings.is_some(),
    }))
}

/// Slice `[start, end]` out of `src`, optionally denoise, and peak-normalise it,
/// returning WAV bytes.
///
/// ffmpeg cuts and sox normalises (`norm -1`), keeping playback loudness
/// stable.
///
/// The optional `enhance` stage sits between them: `deep-filter`
/// (`DeepFilterNet` on tract, which needs no AVX2, which the fleet lacks)
/// writes its output under the input's own name in `-o`'s directory, hence the
/// subdirectory. `-D` compensates the model's lookahead so timestamps stay
/// aligned with the raw clip. It takes about 3.5 s per 10 s of speech, which is
/// why the flag defaults off; see docs/architecture.md.
pub fn render(src: &Path, start: f64, end: f64, enhance: bool) -> std::io::Result<Vec<u8>> {
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
    let feed = if enhance {
        let denoised_dir = dir.path().join("df");
        std::fs::create_dir(&denoised_dir)?;
        let denoised = Command::new("deep-filter")
            .arg("-D")
            .arg("-o")
            .arg(&denoised_dir)
            .arg(&cut)
            .status()?;
        if !denoised.success() {
            return Err(std::io::Error::other("deep-filter failed"));
        }
        denoised_dir.join("clip.wav")
    } else {
        cut
    };
    let normalised = Command::new("sox")
        .arg(&feed)
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
    /// Denoise before normalising — costs seconds of wait; see [`render`].
    #[serde(default)]
    enhance: bool,
}

/// Query for the single-turn route, which takes its id from the path.
#[derive(Deserialize)]
pub struct AudioQuery {
    /// Denoise before normalising — costs seconds of wait; see [`render`].
    #[serde(default)]
    enhance: bool,
    /// Seconds of lead-in and lead-out, replacing the tier's padding: checking
    /// a line's words needs the one Whisper's early end clipped. At most
    /// [`MAX_PAD_S`].
    pad: Option<f64>,
}

/// The widest `pad` a caller may ask for: past it the clip is mostly the
/// neighbouring lines.
const MAX_PAD_S: f64 = 3.0;

/// The window to play for one turn, with the caller's padding if given.
#[must_use]
pub fn padded_window(p: &Placement, pad: Option<f64>) -> (f64, f64) {
    match pad.filter(|pad| pad.is_finite()) {
        Some(pad) => clip_window(p.start_s, p.end_s, pad.clamp(0.0, MAX_PAD_S), 0.0),
        None => window_for(p),
    }
}

/// Why a clip could not be produced.
///
/// Errors as data: the HTTP status is decided at the edge, not inside the
/// picker's query.
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
/// out to ffmpeg and sox: on the request thread it would stall every other
/// request, ingest included, for the length of a clip.
fn clip(path: &Path, start: f64, end: f64, enhance: bool) -> Response {
    match render(path, start, end, enhance) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "audio/wav")], bytes).into_response(),
        Err(err) => crate::route::faulted("clip render", &err),
    }
}

fn no_audio() -> Response {
    (StatusCode::NOT_FOUND, "no audio").into_response()
}

pub type Picked = Result<Option<(PathBuf, f64, f64)>, ClipError>;

/// Open the read-only connection, pick a window and render it: the whole of
/// what an audio route does, and the only caller of `clip`.
///
/// ⚠ Must run on the blocking pool; see `clip`.
pub fn render_blocking(
    root: &Path,
    enhance: bool,
    pick: impl FnOnce(&Connection) -> Picked,
) -> Response {
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
        Ok(Some((path, start, end))) => clip(&path, start, end, enhance),
    }
}

pub async fn audio_route(
    State(st): State<Arc<crate::reads::State>>,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Query(q): Query<AudioQuery>,
) -> Response {
    let root = st.root.clone();
    let rendered = tokio::task::spawn_blocking(move || {
        render_blocking(&root, q.enhance, |conn| {
            Ok(placement(conn, id)?.map(|p| {
                let (start, end) = padded_window(&p, q.pad);
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
        render_blocking(&root, q.enhance, |conn| {
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
