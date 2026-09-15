//! `recall-cli` — the archive from a terminal.
//!
//! ⚠ **Every command asks the FLEET, never the Mac's local `recall.sqlite`.**
//! That file still exists and `refine` still writes it, so it is free to diverge
//! from the record with nothing telling a reader which they got. [`api`] carries
//! the rest of that reasoning.
//!
//! [`render`] holds the display rules and is pure, so they are tested without a
//! fleet to ask.

pub mod api;
pub mod day;
pub mod render;
