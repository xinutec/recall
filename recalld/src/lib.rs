//! recalld — recall's system-of-record daemon on the fleet.
//!
//! Stage A of docs/architecture.md: the ingest plane. Recorders PUT closed
//! segments and verify sha-256 receipts; the store is append-only; read is a
//! separate credential. Later stages add VAD, the room builder, the work
//! queue, and the browsing API.

pub mod app;
pub mod assign;
pub mod audio;
pub mod capture;
pub mod conversations;
pub mod devices;
pub mod ingest;
pub mod instant;
pub mod labels;
pub mod labels_write;
pub mod levels;
pub mod proxy;
pub mod pyjson;
pub mod queue;
pub mod reads;
pub mod reports;
pub mod room;
pub mod route;
pub mod sessions;
pub mod spa;
pub mod speech;
pub mod store;
pub mod tokens;
pub mod upload;
pub mod vad;
pub mod webauth;
pub mod work;
