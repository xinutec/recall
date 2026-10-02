//! A minute transcribed in language runs (`audiocore::language_runs`, #1388).
//!
//! Each speech stretch's language is detected, a minute is cut only where the
//! language changes, and each run is decoded whole in its language. A minute in
//! one language is decoded whole, as before, with that language stated. A
//! minute the fleet has no speech regions for is decoded whole and left to the
//! model, exactly as without runs.

use crate::shim::{self, Shim};
use audiocore::language_runs::{Run, runs, shifted, stretches};
use audiocore::vad::Region;
use serde_json::{Value, json};
use std::path::Path;

/// The rate clips are cut at: Whisper's own, so a slice is not resampled twice.
const RATE: u32 = 16_000;

/// Seconds in `samples` samples at [`RATE`].
#[expect(
    clippy::cast_precision_loss,
    reason = "a clip's sample count is far below 2^52"
)]
fn seconds_in(samples: usize) -> f64 {
    samples as f64 / f64::from(RATE)
}

/// The sample at `seconds`, clamped into a clip of `len` samples.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to [0, len] first"
)]
fn sample_at(seconds: f64, len: usize) -> usize {
    (seconds.clamp(0.0, seconds_in(len)) * f64::from(RATE)) as usize
}

/// One minute's runs' results as one result in the shim's shape: segments in
/// minute time carrying their run's language, and as the minute's language
/// the one its longest run was decoded in.
#[must_use]
pub fn merge(decoded: &[(Run, Value)]) -> Value {
    let mut segments = Vec::new();
    for (run, result) in decoded {
        segments.extend(shifted(result, run.start));
    }
    let language = decoded
        .iter()
        .max_by(|(a, _), (b, _)| (a.end - a.start).total_cmp(&(b.end - b.start)))
        .and_then(|(run, result)| {
            run.language
                .clone()
                .or_else(|| result["language"].as_str().map(String::from))
        });
    json!({ "language": language, "language_confidence": null, "segments": segments })
}

/// Transcribe `clip` in language runs, given where the fleet heard speech.
/// Returns the result as the fleet stores it and its segment count.
///
/// # Errors
/// A shim failure, as [`Shim::transcribe`] reports it. A clip that cannot be
/// cut (no decoder, no scratch space) is decoded whole instead: runs are a
/// refinement, never a reason to lose the minute.
pub fn transcribe(
    shim: &mut Shim,
    clip: &Path,
    regions: &[[f64; 2]],
    scratch: &Path,
    prompt: Option<&str>,
) -> Result<(Value, usize), shim::Error> {
    let whole = |shim: &mut Shim, language: Option<&str>| {
        shim.transcribe_in(clip, language, prompt)
            .map(|a| (a.raw, a.reply.segments.len()))
    };
    if regions.is_empty() {
        return whole(shim, None);
    }
    let Some(pcm) = audiocore::decode::decode_s16(clip, RATE) else {
        tracing::warn!(clip = %clip.display(), "cannot cut the clip; decoding it whole");
        return whole(shim, None);
    };
    let samples = audiocore::decode::to_f32(&pcm);
    let seconds = seconds_in(samples.len());
    let slice = |start: f64, end: f64| {
        let to = sample_at(end, samples.len());
        let from = sample_at(start, samples.len()).min(to);
        &samples[from..to]
    };
    let regions = regions
        .iter()
        .map(|&[start, end]| Region { start, end })
        .collect();
    let mut languages = Vec::new();
    for stretch in stretches(regions) {
        let audio = slice(stretch.start, stretch.end);
        if audio.is_empty() || audiocore::wav::write_mono16(scratch, RATE, audio).is_err() {
            continue;
        }
        let language = match shim.detect_language(scratch) {
            Ok(answer) => Some(answer.reply.language),
            // A stretch the model will not judge joins whatever surrounds it.
            Err(shim::Error::Refused(_)) => None,
            Err(other) => return Err(other),
        };
        languages.push((stretch, language));
    }
    let planned = runs(&languages, seconds);
    if let [only] = planned.as_slice() {
        return whole(shim, only.language.as_deref());
    }
    let mut decoded = Vec::new();
    for run in planned {
        let audio = slice(run.start, run.end);
        if audio.is_empty() {
            continue;
        }
        if audiocore::wav::write_mono16(scratch, RATE, audio).is_err() {
            tracing::warn!("cannot write a run to scratch; decoding the minute whole");
            return whole(shim, None);
        }
        let result = shim
            .transcribe_in(scratch, run.language.as_deref(), prompt)?
            .raw;
        decoded.push((run, result));
    }
    let merged = merge(&decoded);
    let rows = merged["segments"].as_array().map_or(0, Vec::len);
    Ok((merged, rows))
}
