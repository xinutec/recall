//! Energy envelopes: the level-blind, codec-blind fingerprint of when sound
//! happened, and so the tier-1 alignment signal (docs/architecture.md).
//! Decoded at a low rate in coarse buckets, because alignment at this tier
//! needs shape, not fidelity.

/// Samples per second the envelope is computed from. 8 kHz keeps every speech
/// formant that matters for "is there sound now" at a tenth of the decode cost.
pub const DECODE_RATE: u32 = 8000;
/// Seconds per envelope bucket: 100 ms, well inside what tier 2 (onsets)
/// refines, and coarse enough that Opus artefacts vanish.
pub const BUCKET_S: f64 = 0.1;

/// RMS per bucket over s16le mono PCM at `DECODE_RATE`. The final partial
/// bucket is dropped: its different noise statistic would put one misleading
/// point at the end of every stream.
pub fn rms_buckets(pcm: &[u8]) -> Vec<f32> {
    rms_buckets_at(pcm, DECODE_RATE, BUCKET_S)
}

/// The same measurement on any rate/bucket pair, for a caller that needs a
/// finer grain than a stored segment's.
pub fn rms_buckets_at(pcm: &[u8], rate: u32, bucket_s: f64) -> Vec<f32> {
    let samples_per_bucket = (f64::from(rate) * bucket_s) as usize;
    let bytes_per_bucket = 2 * samples_per_bucket;
    pcm.chunks_exact(bytes_per_bucket)
        .map(|bucket| {
            let sum: f64 = bucket
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    let s = f64::from(i16::from_le_bytes(*pair));
                    s * s
                })
                .sum();
            ((sum / samples_per_bucket as f64).sqrt() / 32768.0) as f32
        })
        .collect()
}

/// The dB level of the envelope's `q`-quantile bucket: `q = 0.9` is "what this
/// mic hears when someone talks", `q = 0.1` its floor. `NEG_INFINITY` for an
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
