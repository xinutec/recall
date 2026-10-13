//! Decoding archived segments to PCM. ffmpeg does the codec work, the same
//! binary the segmenters use.

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

/// Decode to s16le mono at the file's native rate: the dead-segment watchdog
/// looks for exact zeros, which resampling would dither.
pub fn decode_native_s16(path: &Path) -> Option<Vec<u8>> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args(["-ac", "1", "-f", "s16le", "-"])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// A segment's native sample rate and channel count. A live segment's header
/// has no duration: decode with [`decode_s16`] and count bytes.
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
    // Order follows the -show_entries list above.
    let rate: i64 = lines.next()?.parse().ok()?;
    let channels: i64 = lines.next()?.parse().ok()?;
    Some((rate, channels))
}

/// s16le bytes to f32 samples in [-1, 1].
pub fn to_f32(pcm: &[u8]) -> Vec<f32> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32768.0)
        .collect()
}
