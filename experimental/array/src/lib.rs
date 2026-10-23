//! The house's microphones treated as one distributed array. Each
//! device samples on its own clock, so alignment is measured from the audio:
//! cross-correlation per frame, then a line per mic for its offset and drift.
//! Coherence after alignment says how much a steered sum can gain, before one
//! is built.

pub mod align;
pub mod coherence;
pub mod timeline;

/// Every capture this measures is 48 kHz.
pub const RATE: f64 = 48_000.0;
