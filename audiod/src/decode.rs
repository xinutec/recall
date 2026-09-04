//! Decoding archived segments onto a shared wall-clock buffer. ffmpeg does the
//! codec work (the same binary the segmenters use); placement by segment name
//! puts every source on one nominal timeline, so what the aligner measures on
//! top is exactly the clock disagreement.

use crate::rebase::{parse_segment_start, segment_glob};
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

/// The window's PCM for one source, placed on the wall clock by segment names
/// (zero-filled where nothing was recorded). s16le mono at `rate`.
pub fn window_pcm(
    root: &Path,
    source: &str,
    start: DateTime<Utc>,
    seconds: usize,
    rate: u32,
) -> Vec<u8> {
    let mut buf = vec![0u8; 2 * rate as usize * seconds];
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
        for (i, byte) in pcm.iter().enumerate() {
            let pos = at + i as i64;
            if pos >= 0 && (pos as usize) < buf.len() {
                buf[pos as usize] = *byte;
            }
        }
    }
    buf
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

/// s16le bytes to f32 samples in [-1, 1].
pub fn to_f32(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
        .collect()
}
