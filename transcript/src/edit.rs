use crate::{ClipId, Instant, SourceId, Span};

/// What a person did, the one kind of data in recall nothing can re-derive
/// (#1911). The log is append-only: an act is never changed, a later act wins
/// its span, and taking one back is an act of its own ([`Act::Retract`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Edit {
    pub id: EditId,
    /// When the person did it.
    pub at: Instant,
    pub act: Act,
}

/// An edit's place in the log; a later id is a later act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct EditId(i64);

impl EditId {
    /// The id the log assigned. Only the store that owns the log calls this.
    pub const fn from_stored(id: i64) -> Self {
        Self(id)
    }

    pub const fn stored(self) -> i64 {
        self.0
    }
}

/// Every kind of thing a person can do to the record. Nothing else is one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Act {
    /// The words said over `span` of `clip`, replacing the model's words over
    /// `over` (the line the person corrected, which may be wider than where
    /// they placed their words; it covers `span`). `checked` means the person
    /// listened and vouches for them, whether or not they changed any.
    Words {
        clip: ClipId,
        span: Span,
        over: Span,
        text: Text,
        checked: bool,
    },
    /// Who spoke over `span` of `clip`. `enrol` false: do not learn this voice
    /// from it (a guest, or a clip judged unusable for a voiceprint).
    Speaker {
        clip: ClipId,
        span: Span,
        name: Name,
        enrol: bool,
    },
    /// Nobody spoke over `span`: whatever the model wrote there was invented.
    NoSpeech { clip: ClipId, span: Span },
    /// Someone spoke over `span`, but the words cannot be made out.
    Unintelligible { clip: ClipId, span: Span },
    /// A whole voice of a session, as diarization labelled it, is this person.
    Voice {
        source: SourceId,
        cluster: String,
        name: Name,
    },
    /// The language a session is spoken in, chosen by a person.
    Language {
        source: SourceId,
        language: Language,
    },
    /// An earlier act, taken back.
    Retract { edit: EditId },
}

/// Words a person wrote: never empty, never padded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct Text(String);

impl Text {
    /// `None` for text that is empty once trimmed: blank words are not words.
    pub fn new(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A person's name as recall knows them: never empty, never padded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Name(String);

impl Name {
    pub fn new(name: &str) -> Option<Self> {
        let trimmed = name.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The languages the household speaks, the only ones a person can pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum Language {
    Dutch,
    English,
}

impl Language {
    /// Whisper's code for it.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Dutch => "nl",
            Self::English => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "nl" => Some(Self::Dutch),
            "en" => Some(Self::English),
            _ => None,
        }
    }
}
