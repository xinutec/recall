//! Every recalld integration test as one binary, so recalld links once. A new
//! test file needs its `mod` here.
//!
//! `include_str!` is relative to the source file (`../fixtures/...`);
//! `Path::new` resolves against the working directory.
#![expect(
    clippy::disallowed_methods,
    reason = "tests seed and read rows with SQL of their own; the ban is for the daemon"
)]

mod align;
mod align_parity;
mod assign;
mod audio;
mod backfill;
mod capture;
mod clips;
mod conversations;
mod devices;
mod diarized;
mod enrol;
mod http;
mod identify_differential;
mod identify_parity;
mod ingest;
mod labels;
mod labels_write;
mod legacy_edits;
mod live_tier;
mod meaning_schema;
mod phone_flac;
mod queue;
mod reads;
mod record_health;
mod rematch;
mod reports;
mod results;
mod retranscribe;
mod script;
mod sessions;
mod sources;
mod spa;
mod speaking_rate;
mod speech;
mod sql;
mod sync;
mod sync_reads;
mod tokens;
mod turn_store;
mod turns;
mod upload;
mod webauth;
mod work;

/// A test's instant as the typed stamp the writers take.
pub fn stamp(raw: &str) -> audiocore::instant::Stamp {
    audiocore::instant::Stamp::parse(raw).expect("an instant")
}
