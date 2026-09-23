//! audiod — the recall audio plane on a recording machine.
//!
//! What it leaves on disk is the whole contract with the rest of the system:
//! segment files under `<root>/<source>/`, the `.alive` marker, the
//! `capture_paused_until` pause file, the capture log
//! ([`audiocore::capture_log`]) and the uploader's receipts. The doctor reads
//! those; the fleet receives the segments by upload.

pub mod beat_relay;
pub mod capture_run;
pub mod events;
pub mod logrotate;
pub mod meter;
pub mod pause;
pub mod pause_mirror;
pub mod rebase;
pub mod segmenter;
pub mod server;
pub mod upload;
pub mod wire;
