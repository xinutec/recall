//! recall's domain, as data (#1911): clips, instants, sources, the edit log,
//! and the lines a clip shows (`render`).
//!
//! Nothing here does IO or reads the clock, and no dependency can
//! (`scripts/check_pure_crate.py`): every function is a function of its
//! arguments.

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
