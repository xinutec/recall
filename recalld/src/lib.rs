//! recalld — recall's system-of-record daemon on the fleet.
//!
//! Stage A of docs/architecture.md: the ingest plane. Recorders PUT closed
//! segments and verify sha-256 receipts; the store is append-only; read is a
//! separate credential. Later stages add VAD, the room builder, the work
//! queue, and the browsing API.

pub mod app;
pub mod ingest;
pub mod store;
pub mod tokens;
