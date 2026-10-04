//! recall's domain, as data (#1911): what a clip is, when things happened, who
//! a source is, what a person did (the edit log), and the lines a clip shows
//! (`render`).
//!
//! ⚠ Pure by construction. Nothing here does IO or reads the clock, and the
//! crate depends on nothing that could (`scripts/check_pure_crate.py`). A
//! function in this crate is a function of its arguments.

mod clip;
mod edit;
mod instant;
mod latin_ranges;
pub mod quality;
pub mod render;
mod source;
mod span;
pub mod text;
pub mod voice;

pub use clip::{Clip, ClipId};
pub use edit::{Act, Edit, EditId, Language, Name, Text};
pub use instant::Instant;
pub use source::SourceId;
pub use span::Span;
