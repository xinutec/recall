//! What a shown line says about itself: who wrote its words, how much
//! processing it has had, and why it is hidden.

/// The model a person's words are recorded under.
pub const HUMAN_MODEL: &str = "human";

/// The live feed's model, as the runner sends it.
pub const LIVE_MODEL: &str = "live";

/// Why a line is hidden: `rendered_lines.hidden_reason`, which only the view
/// writes, one per reason render leaves model text out. Stored and sent to the
/// app as its phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenReason {
    NoWords,
    SilencePhrase,
    RepetitionLoop,
    /// A person's words replaced these.
    Replaced,
    /// A person marked the words as invented.
    NobodySpoke,
    CannotBeMadeOut,
    /// A stretch a person excluded in the app, as if recording had paused.
    Excluded,
    /// An exclusion recovered from the old tables: nobody chose it here, so
    /// the app offers no undo.
    LeftOut,
}

impl HiddenReason {
    pub const ALL: [Self; 8] = [
        Self::NoWords,
        Self::SilencePhrase,
        Self::RepetitionLoop,
        Self::Replaced,
        Self::NobodySpoke,
        Self::CannotBeMadeOut,
        Self::Excluded,
        Self::LeftOut,
    ];

    /// The stored phrase.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoWords => "no words",
            Self::SilencePhrase => "a phrase written over silence",
            Self::RepetitionLoop => "a repetition loop",
            Self::Replaced => "replaced by a person's words",
            Self::NobodySpoke => "nobody spoke",
            Self::CannotBeMadeOut => "cannot be made out",
            Self::Excluded => "excluded",
            Self::LeftOut => "left out",
        }
    }

    pub fn parse(stored: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == stored)
    }
}

impl rusqlite::types::ToSql for HiddenReason {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

impl rusqlite::types::FromSql for HiddenReason {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let stored = value.as_str()?;
        Self::parse(stored).ok_or_else(|| {
            rusqlite::types::FromSqlError::Other(format!("no hidden reason {stored:?}").into())
        })
    }
}

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

impl From<HiddenReason> for HiddenKind {
    fn from(reason: HiddenReason) -> Self {
        match reason {
            HiddenReason::NobodySpoke => Self::NobodySpoke,
            HiddenReason::Excluded => Self::Excluded,
            _ => Self::Other,
        }
    }
}
