//! Measures a raw s16le PCM stream as it is pumped, so a connection leaves
//! evidence of what the device actually sent. Port of
//! `capture.StreamMeter`.

/// |s16| below this is digital silence, not a live mic: a real room's noise
/// floor measures amplitude 10-90 (-69 to -51 dB); a wedged `CoreAudio` read or the
/// pixel9 dead path yields exact zeros / amplitude 1. 2 tolerates dither while
/// never calling a real, quiet room dead.
pub const SILENCE_PEAK: i32 = 2;

/// |s16 sample| at/above this counts as signal (~ -66 dBFS): safely above
/// digital silence and codec dither, safely below any live mic in a quiet room.
const AUDIBLE_FLOOR: i32 = 16;
const S16_FULL_SCALE: f64 = 32768.0;

/// Total bytes, peak level, and when the first *audible* sample arrived — in
/// stream time, so the phone's wall clock can't confuse it. Chunks need not
/// respect sample boundaries; a half sample carries to the next feed.
pub struct StreamMeter {
    byte_rate: u64,
    carry: Option<u8>,
    pub bytes_total: u64,
    pub peak: i32,
    pub first_audible_byte: Option<u64>,
}

impl StreamMeter {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            byte_rate: 2 * u64::from(sample_rate) * u64::from(channels),
            carry: None,
            bytes_total: 0,
            peak: 0,
            first_audible_byte: None,
        }
    }

    /// Meter one chunk; returns the chunk's own peak |sample| so the caller can
    /// act on the instantaneous level (the liveness marker keys off it).
    pub fn feed(&mut self, data: &[u8]) -> i32 {
        // Stream offset of the first byte we will decode this call.
        let start = self.bytes_total - u64::from(self.carry.is_some());
        self.bytes_total += data.len() as u64;
        let mut buf = Vec::with_capacity(data.len() + 1);
        if let Some(byte) = self.carry.take() {
            buf.push(byte);
        }
        buf.extend_from_slice(data);
        if buf.len() % 2 == 1 {
            self.carry = buf.pop();
        }
        let mut chunk_peak = 0i32;
        for (index, pair) in buf.chunks_exact(2).enumerate() {
            let sample = i32::from(i16::from_le_bytes([pair[0], pair[1]])).abs();
            chunk_peak = chunk_peak.max(sample);
            if self.first_audible_byte.is_none() && sample >= AUDIBLE_FLOOR {
                self.first_audible_byte = Some(start + 2 * index as u64);
            }
        }
        self.peak = self.peak.max(chunk_peak);
        chunk_peak
    }

    /// Loudest sample seen, in dBFS; `None` when not one non-zero sample
    /// arrived (pure digital zeros — indistinguishable from no capture path).
    pub fn peak_db(&self) -> Option<f64> {
        if self.peak == 0 {
            return None;
        }
        let db = 20.0 * (f64::from(self.peak) / S16_FULL_SCALE).log10();
        Some((db * 10.0).round() / 10.0)
    }

    /// Stream-time seconds until the first sample at/above the audible floor;
    /// `None` when the whole stream stayed below it (silence).
    pub fn first_audible_s(&self) -> Option<f64> {
        self.first_audible_byte
            .map(|byte| byte as f64 / self.byte_rate as f64)
    }
}
