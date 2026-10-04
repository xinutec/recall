//! Is this transcript text trustworthy?
//!
//! Every rule here is decidable from the text and its own word timings, with
//! no per-device denominator: a phone's noise suppression makes the same
//! utterance measure differently on two microphones, which is what broke every
//! signal that reached outside the turn. The rules run at write time, so a turn
//! they refuse never reaches the read path. The two the doctor also asks are in
//! `audiocore::text`, so writing a turn and restoring one cannot disagree.

pub use audiocore::text::{is_repetition_loop, is_wordless, trim_wordless};
use audiocore::vad::Region;
pub use transcript::quality::{
    FOREIGN_SCRIPT_MAX, NEAR_SILENT_S, SLOW_RATE, foreign_script_ratio, is_foreign_script,
    is_implausibly_slow, is_latin_letter, is_silence_phrase, speaking_rate,
};

/// What the speech pass found in a clip, for the write-time sweep.
#[derive(Debug, Clone, Default)]
pub struct Heard {
    /// Seconds of speech in the whole clip; negative when it could not look.
    pub seconds: Option<f64>,
    /// Where that speech is, seconds from the clip's start. `None` until the
    /// pass has placed it.
    pub regions: Option<Vec<Region>>,
}

impl Heard {
    /// True if `text`, spanning `[start, end)` seconds into the clip, is what
    /// the model writes over silence and the clip heard none there: the whole
    /// clip is near-silent, or no speech falls inside the span.
    ///
    /// On 19 September, 20 of the 21 lines a person marked "nobody spoke" had
    /// no speech inside their span, and every line they vouched for had some.
    /// Other text with none inside is kept: a phone's suppression hides quiet
    /// speech from the detector, and another mic confirmed 30% of such lines.
    #[must_use]
    pub fn invented(&self, text: &str, start: f64, end: f64) -> bool {
        if !is_silence_phrase(text) {
            return false;
        }
        let near_silent = self
            .seconds
            .is_some_and(|s| (0.0..NEAR_SILENT_S).contains(&s));
        let none_here = self
            .regions
            .as_deref()
            .is_some_and(|regions| speech_inside(regions, start, end) <= 0.0);
        near_silent || none_here
    }
}

/// Seconds of `regions` inside `[start, end)`.
#[must_use]
pub fn speech_inside(regions: &[Region], start: f64, end: f64) -> f64 {
    regions
        .iter()
        .map(|r| (r.end.min(end) - r.start.max(start)).max(0.0))
        .sum()
}

/// True if `text` is nothing but one of `names`: "Anna.", " anna ", "Anna!".
///
/// The cost of the vocabulary prompt: it lists the household's names so Whisper
/// spells them right, so on audio it cannot place it reaches for one. Such a
/// turn is refused rather than kept at zero confidence because what it carries
/// is the assertion that a specific person spoke, and no other signal can see
/// it. It costs the real vocative too, which is affordable only on the live
/// tier: the archive pass re-derives the minute with the context to tell the
/// two apart, and supersedes it.
#[must_use]
pub fn is_bare_name(text: &str, names: &[String]) -> bool {
    let bare = trim_wordless(text);
    !bare.is_empty()
        && names
            .iter()
            .any(|name| name.trim().eq_ignore_ascii_case(bare))
}

/// One word as the model timed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Words with their text and timing, out of a stored `word_timings` value, in
/// either of the two stored encodings: recalld's `{s,e,w}`, re-based to the
/// turn, and the ASR shim's verbatim `{start,end,text,probability}`, absolute
/// within the clip. The absolute one must not be compared across turns.
#[must_use]
pub fn timed_words(timings: &str) -> Vec<Word> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str(timings) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|w| {
            let start = w.get("s").or_else(|| w.get("start"))?.as_f64()?;
            let end = w.get("e").or_else(|| w.get("end"))?.as_f64()?;
            let text = w.get("w").or_else(|| w.get("text"))?.as_str()?;
            Some(Word {
                start,
                end,
                text: text.to_owned(),
            })
        })
        .collect()
}

/// Word spans out of a stored `word_timings` value, via [`timed_words`].
#[must_use]
pub fn word_spans(timings: &str) -> Vec<(f64, f64)> {
    timed_words(timings)
        .into_iter()
        .map(|w| (w.start, w.end))
        .collect()
}
