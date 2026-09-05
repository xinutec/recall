//! audiocore — the DSP and segment-grammar library shared by recall's two
//! Rust daemons (docs/architecture.md, stage D1). audiod owns transducer to
//! filesystem on the Mac; recalld owns the fleet's system of record; what
//! they must agree on — the segment-name grammar, decoding, alignment, the
//! STFT, fusion, WAV I/O — lives here exactly once.

pub mod align;
pub mod decode;
pub mod envelope;
pub mod fuse;
pub mod names;
pub mod stft;
pub mod wav;
