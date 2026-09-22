//! audiocore — what recall's Rust crates must not fork (docs/architecture.md).
//!
//! audiod owns transducer-to-filesystem on the Mac, recalld owns the fleet's
//! system of record, and the doctor grades both. Anything more than one of them
//! judges — where a segment's name begins and ends, whether a stretch of audio
//! is speech, whether a turn's text says anything — has to give ONE answer, so
//! it lives here rather than beside a caller.

pub mod align;
pub mod decode;
pub mod envelope;
pub mod instant;
pub mod names;
pub mod text;
pub mod vad;
pub mod wav;
