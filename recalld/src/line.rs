//! What a shown line says about itself: who wrote its words, how much
//! processing it has had, and why it is hidden.

/// The model a person's words are recorded under.
pub const HUMAN_MODEL: &str = "human";

/// The live feed's model, as the runner sends it.
pub const LIVE_MODEL: &str = "live";

/// What `hidden_reason` says when a person marked the words as invented.
pub const NOBODY_SPOKE: &str = "nobody spoke";

/// How much processing a line has had: the tier the app shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, rename = "Tier")]
pub enum Stage {
    Live,
    Transcribed,
    Diarized,
    Corrected,
}

impl Stage {
    /// As the app and `rendered_lines.tier` spell it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Transcribed => "transcribed",
            Self::Diarized => "diarized",
            Self::Corrected => "corrected",
        }
    }

    pub fn parse(stored: &str) -> Option<Self> {
        [
            Self::Live,
            Self::Transcribed,
            Self::Diarized,
            Self::Corrected,
        ]
        .into_iter()
        .find(|s| s.as_str() == stored)
    }
}

/// Which kind of hide a stored `hidden_reason` is, for the app: a person can
/// take back "nobody spoke" from the line; everything else render decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HiddenKind {
    NobodySpoke,
    Other,
}

impl HiddenKind {
    #[must_use]
    pub fn of(stored: &str) -> Self {
        if stored == NOBODY_SPOKE {
            Self::NobodySpoke
        } else {
            Self::Other
        }
    }
}
