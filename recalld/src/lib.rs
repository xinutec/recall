//! recalld — recall's system-of-record daemon on the fleet.
//!
//! The ingest plane (docs/architecture.md, stage A): recorders PUT closed
//! segments and verify sha-256 receipts; the store is append-only; read is a
//! separate credential. On top of it: VAD, the work queue and
//! the browsing API.

pub mod align;
pub mod app;
pub mod assign;
pub mod audio;
pub mod capture;
pub mod conversations;
pub mod devices;
pub mod diarized;
pub mod enrol;
pub mod identify;
pub mod ingest;
pub mod ingest_schema;
pub mod labels;
pub mod labels_write;
pub mod latin_ranges;
pub mod ledger;
pub mod live_tier;
pub mod meaning_schema;
pub mod pyjson;
pub mod quality;
pub mod queue;
pub mod reads;
pub mod rematch;
pub mod reports;
pub mod route;
pub mod sessions;
pub mod sources;
pub mod spa;
pub mod speech;
pub mod store;
pub mod sync;
pub mod tokens;
pub mod turn_store;
pub mod turns;
pub mod upload;
pub mod webauth;
pub mod work;
