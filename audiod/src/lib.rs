//! audiod — the recall audio plane.
//!
//! The Python pipeline and this daemon meet only at the filesystem: segment
//! files under `<root>/<source>/`, the `.alive` marker, the
//! `capture_paused_until` pause file, and two bookkeeping writes into
//! `recall.sqlite`. Everything here preserves those contracts bit for bit —
//! the worker, doctor, loss reconciler and sync must not be able to tell
//! which language wrote a segment.

pub mod capture_run;
pub mod meter;
pub mod pause;
pub mod pause_mirror;
pub mod rebase;
pub mod segmenter;
pub mod server;
pub mod store;
pub mod upload;
pub mod wire;
