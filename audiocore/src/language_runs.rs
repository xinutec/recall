//! A minute split only where its language changes (#1388).
//!
//! Whisper decodes a window in one language, and a minute holding two gets one
//! for both: the minority turns come back TRANSLATED, fluent and wrong. Here a
//! minute's speech is cut into short stretches, each stretch's language is
//! guessed, and neighbouring stretches in one language become a run that is
//! decoded whole, so a run keeps a minute's context and nothing is cut away.
//! Measured by `experimental/playback`: nl/en switching every turn, 33.8%
//! word errors as whole minutes, 7.4% as runs.

use crate::vad::Region;
use serde_json::{Value, json};

/// Where the language is guessed for [`runs`]: a pause between two turns can
/// be well under a second, and a stretch holding an English and a Dutch
/// turn gets one guess, which then decodes the other turn as a TRANSLATION
/// (playback run 2: "de amerikanen en de vrije fransen" came back as "The
/// Americans and the brave Frenchmen"). Finer stretches cost only the guess:
/// a run is still decoded whole.
pub const JOIN_PAUSE_S: f64 = 0.3;
/// A stretch shorter than this is too little for a language guess.
pub const MIN_STRETCH_S: f64 = 1.0;

/// The stretches whose language [`runs`] groups by.
pub fn stretches(regions: Vec<Region>) -> Vec<Region> {
    join_regions(regions, JOIN_PAUSE_S, MIN_STRETCH_S)
}

/// Speech regions joined across pauses shorter than `join`, pieces shorter
/// than `min` absorbed into the one before them (or after, when one opens the
/// minute). The pause between is carried, so each piece is contiguous audio.
pub fn join_regions(regions: Vec<Region>, join: f64, min: f64) -> Vec<Region> {
    let mut joined: Vec<Region> = Vec::new();
    for r in regions {
        match joined.last_mut() {
            Some(last) if r.start - last.end < join => last.end = r.end,
            _ => joined.push(r),
        }
    }
    let mut merged: Vec<Region> = Vec::new();
    for piece in joined {
        match merged.last_mut() {
            Some(last) if piece.seconds() < min || last.seconds() < min => {
                last.end = piece.end;
            }
            _ => merged.push(piece),
        }
    }
    merged
}

/// A stretch of a minute decoded as one, in one language.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub start: f64,
    pub end: f64,
    /// `None`: no stretch said, so the model detects it.
    pub language: Option<String>,
}

/// Neighbouring stretches in the same language joined into runs that tile the
/// whole minute, `[0, minute)`: a boundary sits mid-pause between two
/// languages, so no audio is cut away, which is what loses quiet words when
/// only the stretches are decoded. A minute with no stretches is one run.
pub fn runs(stretches: &[(Region, Option<String>)], minute: f64) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let mut previous_end = 0.0;
    for (piece, language) in stretches {
        match out.last_mut() {
            Some(last) if last.language == *language => {}
            Some(last) => {
                let boundary = f64::midpoint(previous_end, piece.start);
                last.end = boundary;
                out.push(Run {
                    start: boundary,
                    end: minute,
                    language: language.clone(),
                });
            }
            None => out.push(Run {
                start: 0.0,
                end: minute,
                language: language.clone(),
            }),
        }
        previous_end = piece.end;
    }
    if out.is_empty() {
        out.push(Run {
            start: 0.0,
            end: minute,
            language: None,
        });
    }
    out
}

/// A run's (or piece's) transcription moved into minute time: every segment
/// and word start and end shifted by `offset`, and the result's language kept
/// on each segment, since one minute's segments can now differ in language.
pub fn shifted(result: &Value, offset: f64) -> Vec<Value> {
    let language = result.get("language").cloned().unwrap_or(Value::Null);
    let shift = |v: &mut Value| {
        for key in ["start", "end"] {
            if let Some(t) = v[key].as_f64() {
                v[key] = json!(t + offset);
            }
        }
    };
    let mut out = Vec::new();
    for seg in result["segments"].as_array().into_iter().flatten() {
        let mut seg = seg.clone();
        shift(&mut seg);
        if let Some(words) = seg.get_mut("words").and_then(Value::as_array_mut) {
            words.iter_mut().for_each(shift);
        }
        seg["language"] = language.clone();
        out.push(seg);
    }
    out
}
