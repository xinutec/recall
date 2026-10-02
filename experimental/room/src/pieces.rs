//! Cutting a minute at its pauses, so each piece gets its own language guess.
//!
//! A minute is not a unit of speech, and a block that straddles two languages
//! gets one label for both, which collapses the minority. On read speech every
//! piece came back in its own language and every line complete (#1388).

use audiocore::language_runs::join_regions;
use audiocore::vad::Region;

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
    join_regions(regions, JOIN_PAUSE_S, MIN_PIECE_S)
}
