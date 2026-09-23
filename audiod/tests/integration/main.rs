//! Every audiod integration test, as one binary: cargo builds and links one
//! binary per `tests/*.rs`, so one binary relinks once per change.
//!
//! `include_str!` is relative to its source file, so fixture paths here climb
//! one extra level. A new test file must be added below; cargo does not
//! auto-discover inside this directory.

mod beat_relay;
mod capture;
mod capture_argv;
mod events;
mod ingest;
mod meter;
mod pause;
mod pause_mirror;
mod rebase;
mod segmenter;
mod upload;
mod upload_real_server;
mod usage;
mod wire;
mod wire_fixture;
