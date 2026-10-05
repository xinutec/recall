use crate::{ClipId, Instant, SourceId, Span};

/// What a person did: the one kind of data nothing can re-derive (#1911). The
/// log is append-only: a later act wins its span, and taking one back is an act
/// of its own ([`Act::Retract`]).
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
    /// For the store that owns the log.
    pub const fn from_stored(id: i64) -> Self {
        Self(id)
    }

    pub const fn stored(self) -> i64 {
        self.0
    }
}

/// Everything a person can do to the record.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Act {
    /// The words said over `span`, replacing the model's words over `over`:
    /// the line the person corrected, which covers `span` and may be wider.
    /// `checked`: the person listened and vouches for them, changed or not.
    Words {
        clip: ClipId,
        span: Span,
        over: Span,
        text: Text,
        checked: bool,
    },
    /// Who spoke over `span`. `enrol` false: do not learn a voiceprint from it
    /// (a guest, or unusable audio).
    Speaker {
        clip: ClipId,
        span: Span,
        name: Name,
        enrol: bool,
    },
    /// Nobody spoke over `span`: the model's words there were invented.
    NoSpeech { clip: ClipId, span: Span },
    /// Someone spoke over `span`, but the words cannot be made out.
    Unintelligible { clip: ClipId, span: Span },
    /// A diarized voice of a session is this person.
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
    /// `None` for text that is empty once trimmed.
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
