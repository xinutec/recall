use crate::{Instant, SourceId};

/// One stored recording: a file a recorder delivered or a person uploaded.
///
/// The file is the identity. Two files can share a source and a start (a
/// phone's own copy and the Mac's cut of its stream often open in the same
/// second), so `(source, start)` is not a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct ClipId(i64);

impl ClipId {
    /// The id a store assigned. Only the store that owns clip rows should call
    /// this; everything else receives ids from it.
    pub const fn from_stored(id: i64) -> Self {
        Self(id)
    }

    pub const fn stored(self) -> i64 {
        self.0
    }
}

/// A clip as the domain sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Clip {
    pub id: ClipId,
    pub source: SourceId,
    /// When recording began, by the recorder's clock, from the file's name.
    pub start: Instant,
    /// The file's name. A rename (`.wav` to `.phone.flac`) keeps the id.
    pub filename: String,
    /// Where the file is, relative to the data root.
    pub path: String,
}
