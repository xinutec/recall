//! What a shown line says about itself: who wrote its words, how much
//! processing it has had, and why it is hidden.

/// The model a person's words are recorded under.
pub const HUMAN_MODEL: &str = "human";

/// The live feed's model, as the runner sends it.
pub const LIVE_MODEL: &str = "live";

/// What `hidden_reason` says when a person marked the words as invented.
pub const NOBODY_SPOKE: &str = "nobody spoke";

/// What `hidden_reason` says for a stretch a person excluded in the app, as
/// if recording had paused. The hides recovered from the old tables read
/// "left out" instead: nobody chose them here, so the app offers no undo.
pub const EXCLUDED: &str = "excluded";

/// A shown line's id (`rendered_lines.id`), what the app sends back to act
/// on a line. Not a [`transcript::EditId`], which names a person's act: the
/// two are both integers and one in the other's place acts on the wrong row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(transparent)]
pub struct LineId(i64);

impl LineId {
    /// For the view that owns the rows, and a request naming one.
    pub const fn from_stored(id: i64) -> Self {
        Self(id)
    }

    pub const fn stored(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for LineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

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
/// take back "nobody spoke" from the line, and an excluded stretch from its
/// row; everything else render decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum HiddenKind {
    NobodySpoke,
    Excluded,
    Other,
}

impl HiddenKind {
    /// The hide a stored `hidden_reason` is: a person's, or render's.
    #[must_use]
    pub fn of(stored: &str) -> Self {
        Self::by_person(stored).unwrap_or(Self::Other)
    }

    /// The hides a person makes, which are a closed set; render's reasons are
    /// not, and are all [`Self::Other`].
    fn by_person(stored: &str) -> Option<Self> {
        match stored {
            NOBODY_SPOKE => Some(Self::NobodySpoke),
            EXCLUDED => Some(Self::Excluded),
            _ => None,
        }
    }
}
