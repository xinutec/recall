//! recalld: recall's system of record on the fleet. The ingest plane
//! (append-only, receipted uploads), and on top of it speech detection, the
//! work queue, the passes and the browsing API (docs/architecture.md).

pub mod app;
pub mod audio;
pub mod capture;
pub mod clips;
pub mod conversations;
pub mod devices;
pub mod edit_log;
pub mod enrol;
pub mod forget;
pub mod identify;
pub mod ingest;
pub mod ingest_schema;
pub mod labels;
pub mod labels_write;
pub mod ledger;
pub mod line;
pub mod live_tier;
pub mod meaning_schema;
pub mod passes;
pub mod quality;
pub mod queue;
pub mod reads;
pub mod record_health;
pub mod registrar;
pub mod rendering;
pub mod reports;
pub mod results;
pub mod retranscribe;
pub mod route;
pub mod same_speech;
pub mod sessions;
pub mod settings;
pub mod sources;
pub mod spa;
pub mod speech;
pub mod sql;
pub mod store;
pub mod sync;
pub mod tokens;
pub mod upload;
pub mod verify;
pub mod view;
pub mod webauth;
pub mod work;
