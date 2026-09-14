//! The Mac's two Rust agents and what they share.
//!
//! `runner` (bin, `main.rs`) leases a job, fetches its audio, drives a model
//! shim, pushes the result, acks. `recall-live` (bin, `bin/recall-live.rs`)
//! reads the microphone tap, cuts it at the pauses and pushes each utterance
//! the moment it ends. They are the same shape: drive a shim, push to Isis.
//!
//! ⚠ **Neither holds state, and that is the point of both.** No watermark, no
//! outbox, no mirror queue, no local store — because the queue and the record
//! live on Isis. Kill either at any moment and nothing needs recovering: the
//! runner's lease expires, and live loses the sentence being said.
//!
//! One job at a time per shim, which holds a model and cannot usefully be asked
//! two things at once.

pub mod client;
pub mod live;
pub mod pulse;
pub mod shim;
