//! `recall-cli`: the archive from a terminal. Every command asks recalld; none
//! reads a local database. [`render`] is pure, so the display rules are tested
//! without a server.

pub mod api;
pub mod day;
pub mod render;
