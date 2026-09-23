//! Every recalld integration test, as one binary: cargo links one binary per
//! `tests/*.rs`, each carrying all of recalld, so one binary means one link.
//!
//! ⚠ `include_str!` is relative to its source file, so fixture paths here are
//! `../fixtures/...`. `Path::new("../tests/fixtures/...")` resolves against the
//! working directory at runtime instead, and stays as it is.
//!
//! Adding a test file means adding its `mod` here; cargo does not auto-discover
//! inside this directory.

mod align;
mod align_parity;
mod assign;
mod audio;
mod capture;
mod conversations;
mod devices;
mod diarized;
mod enrol;
mod identify_differential;
mod identify_parity;
mod ingest;
mod labels;
mod labels_write;
mod levels;
mod live_tier;
mod meaning_schema;
mod processed;
mod queue;
mod reads;
mod rematch;
mod reports;
mod room;
mod script;
mod sessions;
mod sources;
mod spa;
mod speaking_rate;
mod speech;
mod sync;
mod sync_reads;
mod tokens;
mod turns;
mod upload;
mod webauth;
mod work;
