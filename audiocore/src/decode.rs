//! Decoding archived segments onto a shared wall-clock buffer. ffmpeg does the
//! codec work (the same binary the segmenters use); placement by segment name
//! puts every source on one nominal timeline, so what the aligner measures on
//! top is exactly the clock disagreement.

use crate::names::{parse_segment_start, segment_glob};
use chrono::{DateTime, Duration, Utc};
use std::path::Path;

/// Decode one archived segment to s16le mono at `rate`.
pub fn decode_s16(path: &Path, rate: u32) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-ac", "1", "-ar", &rate.to_string(), "-f", "s16le", "-"])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// A window of a source's audio, and how much of it was really there.
pub struct Window {
    /// s16le mono at the requested rate, zero-filled where nothing was recorded.
    pub pcm: Vec<u8>,
    /// Fraction a stored clip actually covered, 0.0..=1.0. Once mixed into one
    /// buffer the zero-fill is indistinguishable from silence, so only this
    /// separates "the room was quiet" from "nothing was recorded" (#1661).
    pub coverage: f32,
}

/// The window's PCM for one source, placed on the wall clock by segment names
/// (zero-filled where nothing was recorded). s16le mono at `rate`.
///
/// Prefer [`window_covered`] where the result is stored or transcribed: this
/// spelling cannot say whether the audio was there.
#[must_use]
pub fn window_pcm(
    root: &Path,
    source: &str,
    start: DateTime<Utc>,
    seconds: usize,
    rate: u32,
) -> Vec<u8> {
    window_covered(root, source, start, seconds, rate).pcm
}

/// The window, with the fraction of it a stored clip actually covered.
#[must_use]
pub fn window_covered(
    root: &Path,
    source: &str,
    start: DateTime<Utc>,
    seconds: usize,
    rate: u32,
) -> Window {
    let mut buf = vec![0u8; 2 * rate as usize * seconds];
    // Union of the spans, not a count of writes: two clips overlapping the same
    // instant cover it once.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for path in segment_glob(&root.join(source), source) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(seg_start) = parse_segment_start(name) else {
            continue;
        };
        // Generously admit anything that could intersect the window.
        if seg_start < start - Duration::seconds(120)
            || seg_start > start + Duration::seconds(seconds as i64)
        {
            continue;
        }
        let Some(pcm) = decode_s16(&path, rate) else {
            continue;
        };
        let shift = (seg_start - start).num_milliseconds() as f64 / 1000.0;
        let at = (shift * f64::from(rate)) as i64 * 2;
        let mut lowest = buf.len();
        let mut highest = 0usize;
        for (i, byte) in pcm.iter().enumerate() {
            let pos = at + i as i64;
            if pos >= 0 && (pos as usize) < buf.len() {
                let pos = pos as usize;
                buf[pos] = *byte;
                lowest = lowest.min(pos);
                highest = highest.max(pos + 1);
            }
        }
        if lowest < highest {
            spans.push((lowest, highest));
        }
    }
    let coverage = if buf.is_empty() {
        0.0
    } else {
        union_len(&mut spans) as f32 / buf.len() as f32
    };
    Window { pcm: buf, coverage }
}

/// Total length covered by `spans`, counting an overlap once.
fn union_len(spans: &mut [(usize, usize)]) -> usize {
    spans.sort_unstable();
    let mut total = 0usize;
    let mut open: Option<(usize, usize)> = None;
    for &(from, to) in spans.iter() {
        match open {
            Some((start, end)) if from <= end => open = Some((start, end.max(to))),
            Some((start, end)) => {
                total += end - start;
                open = Some((from, to));
            }
            None => open = Some((from, to)),
        }
    }
    if let Some((start, end)) = open {
        total += end - start;
    }
    total
}

/// Decode to s16le mono at the file's native rate — the dead-segment
/// watchdog's input, where forcing a rate would resample and dither the exact
/// zeros it is looking for.
pub fn decode_native_s16(path: &Path) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-ac", "1", "-f", "s16le", "-"])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// A segment's native sample rate and channel count, from the stream header.
///
/// ⚠ **The header carries no DURATION**, and that is measured rather than
/// assumed: `ffprobe -show_entries format=duration` on a live `usb-*.flac`
/// returns an empty object (checked 2026-09-17). A caller that needs length has
/// to decode — [`decode_s16`] and divide the byte count by the rate it asked
/// for.
///
/// ⚠ Two plain lines, not JSON, so this crate needs no serialiser: `audiocore`
/// is linked into every binary here and a dependency added for two integers
/// would be paid by all of them.
#[must_use]
pub fn stream_shape(path: &Path) -> Option<(i64, i64)> {
    let out = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            "-show_entries",
            "stream=sample_rate,channels",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.split_whitespace();
    // Order follows the -show_entries list, which is why it is spelled there and
    // read here in one place rather than assumed at each call site.
    let rate: i64 = lines.next()?.parse().ok()?;
    let channels: i64 = lines.next()?.parse().ok()?;
    Some((rate, channels))
}

/// s16le bytes to f32 samples in [-1, 1].
pub fn to_f32(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
        .collect()
}
