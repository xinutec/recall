//! The Mac worker (stage E3): lease a job, fetch its audio, drive a model shim,
//! push the result, ack.
//!
//! This is the whole Mac orchestration the architecture asks for. It replaces
//! worker, live, jobs, sync-push, outbox and capture-mirror with a poller that
//! holds NO STATE: no watermark, no outbox, no mirror queue, because the queue
//! lives on Isis. If the Mac dies, nothing here needs recovering — the lease
//! simply expires and the job returns to the pool.
//!
//! One job at a time, matching the shim, which holds a model and cannot
//! usefully be asked two things at once.

pub mod client;
pub mod shim;
