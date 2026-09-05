//! Energy envelopes: the level-blind, codec-blind fingerprint of *when sound
//! happened*, and therefore the tier-1 alignment signal (docs/audio-plane.md).
//! Decoded at a low rate in coarse buckets — the same resolution the Python
//! side draws timelines at (`recall.envelope`) — because alignment at this
//! tier needs shape, not fidelity.

/// Samples per second the envelope is computed from. 8 kHz keeps every speech
/// formant that matters for "is there sound now" at a tenth of the decode cost.
pub const DECODE_RATE: u32 = 8000;
/// Seconds per envelope bucket: 100 ms — an alignment resolution well inside
/// what tier 2 (onsets) refines, and coarse enough that Opus artefacts vanish.
pub const BUCKET_S: f64 = 0.1;

/// RMS per bucket over s16le mono PCM at `DECODE_RATE`. The final partial
/// bucket is dropped — a shorter bucket has a different noise statistic and
/// would put one misleading point at the end of every stream.
pub fn rms_buckets(pcm: &[u8]) -> Vec<f32> {
    rms_buckets_at(pcm, DECODE_RATE, BUCKET_S)
}

/// The same measurement on any rate/bucket pair — the fusion driver reuses it
/// at 10 ms buckets for fine alignment.
pub fn rms_buckets_at(pcm: &[u8], rate: u32, bucket_s: f64) -> Vec<f32> {
    let samples_per_bucket = (f64::from(rate) * bucket_s) as usize;
    let bytes_per_bucket = 2 * samples_per_bucket;
    pcm.chunks_exact(bytes_per_bucket)
        .map(|bucket| {
            let sum: f64 = bucket
                .chunks_exact(2)
                .map(|pair| {
                    let s = f64::from(i16::from_le_bytes([pair[0], pair[1]]));
                    s * s
                })
                .sum();
            ((sum / samples_per_bucket as f64).sqrt() / 32768.0) as f32
        })
        .collect()
}

/// The dB level of the envelope's `q`-quantile bucket — the one measurement
/// stage D's calibration stores per segment: `q = 0.9` is "what this mic
/// hears when someone talks", `q = 0.1` its floor, the same quantiles the
/// fusion bake-off ranked with (docs/audio-plane.md). `NEG_INFINITY` for an
/// empty envelope, so an absent segment never reads as a quiet one.
pub fn level_quantile_db(envelope: &[f32], q: f64) -> f32 {
    if envelope.is_empty() {
        return f32::NEG_INFINITY;
    }
    let mut sorted = envelope.to_vec();
    sorted.sort_by(f32::total_cmp);
    let level = sorted[((sorted.len() - 1) as f64 * q) as usize];
    20.0 * level.max(1e-9).log10()
}
