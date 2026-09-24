//! audiocore: what recall's Rust crates must not fork (docs/architecture.md).
//!
//! audiod owns transducer-to-filesystem, recalld owns the fleet's system of
//! record, and the doctor grades both. Anything more than one of them judges
//! (a segment's name, whether audio is speech, whether a turn's text says
//! anything) lives here so it has one answer.

pub mod align;
pub mod capture_log;
pub mod decode;
pub mod envelope;
pub mod instant;
pub mod job;
pub mod names;
pub mod text;
pub mod vad;
pub mod wav;
