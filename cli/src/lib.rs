//! `recall-cli` — the archive from a terminal, over the fleet's own API.
//!
//! ⚠ **The point of this crate is WHICH DATABASE it asks.** `recall.cli` opened
//! the Mac's `recall.sqlite` and answered from it. That was right while the Mac
//! was the system of record and has been wrong since Isis became it: the local
//! file is now a copy that `refine` still writes and nothing reconciles, so the
//! two can disagree — and on 2026-09-15 they did, the local copy's newest turn
//! being five days behind the fleet's. Every command here goes through
//! `http://10.100.0.2:8000`, which is the record itself.
//!
//! So this is a port that also FIXES something, and the fix is the reason to do
//! it rather than a side effect: a person who searches their own archive and
//! gets a stale answer has no way to tell.
//!
//! The split is the usual one — [`api`] does the asking, [`render`] does the
//! formatting and holds every display rule, and `main` does the arguments.
//! [`render`] is pure, so the rules are tested without a fleet.

pub mod api;
pub mod day;
pub mod render;
