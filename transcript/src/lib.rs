//! recall's domain, as data (#1911): what a clip is, when things happened, and
//! who a source is. Later stages add the edit log and `render`.
//!
//! ⚠ Pure by construction. Nothing here does IO or reads the clock, and the
//! crate depends on nothing that could (`scripts/check_pure_crate.py`). A
//! function in this crate is a function of its arguments.

mod clip;
mod instant;
mod source;
mod span;

pub use clip::{Clip, ClipId};
pub use instant::Instant;
pub use source::SourceId;
pub use span::Span;
