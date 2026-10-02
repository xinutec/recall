//! The messages between the runner and the model shims, defined once.
//!
//! The runner builds its requests from these, checks each reply parses before
//! forwarding it, and the fleet reads the stored results through them. The
//! Python shims are held to the same shapes by committed examples
//! (`tests/fixtures/shim/`): the Python tests produce and accept exactly those
//! files, and `audiocore/tests/shim.rs` reads and writes them with these types.
//!
//! On the wire a request is one JSON object: `id`, `op` and the op's arguments,
//! flat. A reply is `{"id", "ok": true, "result": …}` or `{"id", "ok": false,
//! "error": …}`; the runner stores `{"ok", "result" | "error"}` as the job's
//! result, which [`Stored`] reads back.

use serde::{Deserialize, Serialize};

/// A job result as the fleet stores it: the shim's answer, or why it refused.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Stored<T> {
    pub ok: bool,
    // No `default`: on a generic it would ask `T: Default`, and an absent
    // `Option` field reads as `None` anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: for<'de> Deserialize<'de>> Stored<T> {
    /// Read a stored job result.
    ///
    /// # Errors
    /// When the text is not this shape: a result from a shim this code does
    /// not understand.
    pub fn parse(stored: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(stored)
    }

    /// The answer, if the shim gave one.
    pub fn answer(self) -> Option<T> {
        if self.ok { self.result } else { None }
    }
}

/// A shim's answer on the wire: the answer, or why it refused. `result` stays
/// JSON here, because the runner keeps it as sent; each op's type reads it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Response {
    #[serde(default)]
    pub id: Option<String>,
    pub ok: bool,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The answer to `hello`, which the protocol layer gives even when the model
/// failed to load.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Hello {
    pub shim: String,
}

/// Transcription (`asr` shim, op `transcribe`).
pub mod asr {
    use serde::{Deserialize, Serialize};

    /// The op name.
    pub const OP: &str = "transcribe";
    /// The language-only op: one encoder pass and one decoder step.
    pub const DETECT: &str = "detect-language";

    /// The language spoken in one clip's first 30 s.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Detect {
        pub audio: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model: Option<String>,
    }

    /// The most probable language and its probability.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Detected {
        pub language: String,
        pub probability: f64,
    }

    /// Transcribe one clip.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Request {
        pub audio: String,
        /// Word timings, which alignment to speakers needs.
        pub words: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// The household vocabulary, carried because a shim reads no database.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub initial_prompt: Option<String>,
    }

    /// What the model heard.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Reply {
        pub language: Option<String>,
        #[serde(default)]
        pub language_confidence: Option<f64>,
        #[serde(default)]
        pub segments: Vec<Segment>,
    }

    /// One transcribed span.
    ///
    /// ⚠ `start`/`end` are optional because results were stored without them;
    /// a reader that needs them says what it does when they are missing.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Segment {
        pub start: Option<f64>,
        pub end: Option<f64>,
        #[serde(default)]
        pub text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub avg_logprob: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub no_speech_prob: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub confidence: Option<f64>,
        /// This segment's language, when a result decoded its minute in runs
        /// of different languages; absent, the reply's `language` holds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub language: Option<String>,
        /// `None` when the result carries no word timings at all, which is not
        /// the same as a segment with none.
        #[serde(default)]
        pub words: Option<Vec<Word>>,
    }

    /// One word, timed from the clip's start.
    ///
    /// Older results spell `text` as mlx-whisper's `word` and carry no
    /// probability; both are read, and the current spelling is written.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Word {
        pub start: f64,
        pub end: f64,
        #[serde(alias = "word")]
        pub text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub probability: Option<f64>,
    }
}

/// Speakers (`voices` shim, ops `diarize` and `embed`).
pub mod voices {
    use serde::{Deserialize, Serialize};

    /// The diarize op's name.
    pub const DIARIZE: &str = "diarize";
    /// The embed op's name.
    pub const EMBED: &str = "embed";

    /// Separate one clip's speakers, and with `embed` build a print for each.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Diarize {
        pub audio: String,
        pub embed: bool,
    }

    /// Who spoke when, and each speaker's print when asked for.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Diarization {
        #[serde(default)]
        pub turns: Vec<SpeakerTurn>,
        /// Absent in results stored before the shim embedded.
        #[serde(default)]
        pub speakers: Vec<SpeakerVoice>,
    }

    /// A contiguous span attributed to one relative speaker, clip-relative.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct SpeakerTurn {
        pub speaker: String,
        pub start: f64,
        pub end: f64,
    }

    /// One speaker's print, made from their longest turn in the clip.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct SpeakerVoice {
        pub speaker: String,
        /// How much audio the print was made from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seconds: Option<f64>,
        pub vector: Vec<f64>,
    }

    /// Embed one span of a clip (seconds from its start).
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Embed {
        pub audio: String,
        pub start: f64,
        pub end: f64,
    }

    /// A print, or none for audio too short or broken to embed.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Embedding {
        pub vector: Option<Vec<f64>>,
    }

    /// An enrolment job's result: the runner embeds each span it was given
    /// and sends back the prints, one per span that embedded.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Prints {
        #[serde(default)]
        pub prints: Vec<Print>,
    }

    /// One embedded span, by the turn it was cut from.
    #[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
    pub struct Print {
        pub segment_id: i64,
        pub vector: Vec<f64>,
    }
}
