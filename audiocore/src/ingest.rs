//! The ingest plane's receipt, one type for recalld and the uploader. The
//! bytes are pinned by `tests/fixtures/wire/`.

use serde::{Deserialize, Serialize};

/// A PUT's answer: what recalld stored, for the uploader to check against what
/// it sent before anything counts as delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub sha256: String,
    pub bytes: usize,
}
