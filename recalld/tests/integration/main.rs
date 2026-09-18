//! Every recalld integration test, as ONE binary.
//!
//! ⚠ **This is a BUILD-COST change and nothing else.** Each file below was its
//! own `tests/*.rs`, and cargo builds one binary per such file — each linking
//! the whole of recalld at 35-51 MB. Thirty of them meant thirty links every
//! time a single source file changed, and measured 2026-09-13 against the
//! gate's own record: `cargo test (workspace)` cost 177-440 s while the actual
//! test EXECUTION across all 75 suites was 18.7 s. Over 90% of it was linking.
//!
//! ⚠ **Nothing about the tests themselves changes**, and the check that this
//! stayed true is a name-by-name diff of every `#[test]` in the tree before and
//! after — 350 of them. A consolidation that quietly dropped a file would
//! otherwise look exactly like a speedup.
//!
//! ⚠ **`include_str!` is relative to its SOURCE FILE**, so every fixture path
//! here climbs one level (`../fixtures/...`) that did not need to before.
//! `Path::new("../tests/fixtures/...")` is NOT the same thing — that one
//! resolves against the working directory at runtime and was left alone.
//!
//! Adding a test file means adding it here too; cargo does not auto-discover
//! inside this directory, which is the whole point.

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
mod meaning_schema;
mod processed;
mod proxy;
mod quality_parity;
mod queue;
mod reads;
mod rematch;
mod reports;
mod room;
mod segments;
mod sessions;
mod sources;
mod spa;
mod speech;
mod sync;
mod sync_reads;
mod tokens;
mod turns;
mod upload;
mod webauth;
mod work;
