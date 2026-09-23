//! `recall-cli`: the archive from a terminal.
//!
//! Every command asks the fleet, the system of record, never a local database
//! (see [`api`]).
//!
//! [`render`] holds the display rules and is pure, so they are tested without a
//! fleet to ask.

pub mod api;
pub mod day;
pub mod render;
