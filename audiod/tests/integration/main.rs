//! Every audiod integration test, as ONE binary. See
//! `recalld/tests/integration/main.rs` for why: cargo builds one binary per
//! `tests/*.rs`, each linking the whole crate, so touching one source file
//! relinked fourteen of them.
//!
//! ⚠ `common` is declared HERE and nowhere else. It used to be declared by each
//! file that wanted it, which a single binary cannot do twice; the files that
//! want it now say `use crate::common;`, so every call site reads unchanged.
//!
//! ⚠ `include_str!` is relative to its SOURCE FILE, so fixture paths here climb
//! one level that they did not need to before.
//!
//! Adding a test file means adding it here too; cargo does not auto-discover
//! inside this directory, which is the whole point.

mod common;

mod beat_relay;
mod capture;
mod capture_argv;
mod ingest;
mod meter;
mod pause;
mod pause_mirror;
mod rebase;
mod register;
mod segmenter;
mod speech_scan;
mod store;
mod upload;
mod upload_real_server;
mod usage;
mod wire;
mod wire_fixture;
