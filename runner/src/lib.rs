//! The Mac's two Rust agents and what they share.
//!
//! `runner` (`main.rs`) leases a job, fetches its audio, drives a model shim
//! and pushes the result. `recall-live` (`bin/recall-live.rs`) reads the
//! microphone tap, cuts it at the pauses and pushes each utterance as it ends.
//!
//! Neither holds state; the queue and the record live on Isis. Killing either
//! costs at most a lease that expires or the sentence being said.
//!
//! One job at a time per shim, because a shim holds one model.

pub mod client;
pub mod live;
pub mod pulse;
pub mod shim;
