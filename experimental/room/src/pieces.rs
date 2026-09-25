//! Cutting a minute at its pauses, so each piece gets its own language guess.
//!
//! A minute is not a unit of speech, and a block that straddles two languages
//! gets one label for both, which collapses the minority. On read speech every
//! piece came back in its own language and every line complete (#1388).

use audiocore::vad::Region;
use serde_json::{Value, json};

/// A pause shorter than this is within an utterance, not between two. Matched
/// to the live tier's bridge, the only pause length this system has measured.
pub const JOIN_PAUSE_S: f64 = 2.0;

/// A piece shorter than this is a fragment and joins its neighbour. On 22
/// blocks of household conversation, 12 of 58 pieces were under two seconds: a
/// listener's "mm" between two turns is a region of its own.
pub const MIN_PIECE_S: f64 = 3.0;

/// Speech regions joined across short pauses, fragments absorbed into the piece
/// before them (or after, when a fragment opens the minute). The pause between
/// is carried, so each piece is contiguous audio.
///
/// A minute whose only region is a fragment keeps it: there is no neighbour.
pub fn pieces(regions: Vec<Region>) -> Vec<Region> {
    let mut joined: Vec<Region> = Vec::new();
    for r in regions {
        match joined.last_mut() {
            Some(last) if r.start - last.end < JOIN_PAUSE_S => last.end = r.end,
            _ => joined.push(r),
        }
    }
    let mut merged: Vec<Region> = Vec::new();
    for piece in joined {
        match merged.last_mut() {
            Some(last) if piece.seconds() < MIN_PIECE_S || last.seconds() < MIN_PIECE_S => {
                last.end = piece.end;
            }
            _ => merged.push(piece),
        }
    }
    merged
}

/// A piece's transcription moved from piece time to block time: every segment
/// and word start and end shifted by `offset`, and the piece's own language
/// guess kept on each segment, since that is the point of cutting.
pub fn in_block_time(result: &Value, offset: f64) -> Vec<Value> {
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
