//! audiod — the recall audio plane.
//!
//! The Python pipeline and this daemon meet only at the filesystem: segment
//! files under `<root>/<source>/`, the `.alive` marker, the
//! `capture_paused_until` pause file, and two bookkeeping writes into
//! `recall.sqlite`. Everything here preserves those contracts bit for bit —
//! the worker, doctor, loss reconciler and sync must not be able to tell
//! which language wrote a segment.

pub mod align;
pub mod capture_run;
pub mod decode;
pub mod envelope;
pub mod fuse;
pub mod meter;
pub mod pause;
pub mod rebase;
pub mod segmenter;
pub mod server;
pub mod stft;
pub mod store;
pub mod upload;
pub mod wav;
pub mod wire;
