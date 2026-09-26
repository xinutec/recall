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

/// Measured speech under which a minute counts as near-silent, in seconds. The
/// speech pass's floor is one 0.256 s blip, and in minutes under a second the
/// commonest lines on this archive were "Thank you." and video sign-offs
/// (2026-09-26, #1461).
pub const NEAR_SILENT_S: f64 = 1.0;

/// What Whisper writes over silence: thanks in several languages, "you", "bye",
/// and the sign-offs of the videos it was trained on. Lowercased, punctuation
/// gone, one space between words.
const SILENCE_PHRASES: &[&str] = &[
    "you",
    "bye",
    "see you next time",
    "i'll see you next time",
    "thanks for watching",
    "dank u wel",
    "vielen dank",
    "gracias",
    "obrigado",
    "merci",
    "продолжение следует",
    "ご視聴ありがとうございました",
];

/// Words a "thank you" can be made of, repeated or cut short by the model.
const THANKS_WORDS: &[&str] = &["thank", "thanks", "you", "very", "much", "so"];

/// True if `text` is a phrase the model writes over silence (see
/// [`SILENCE_PHRASES`]), or thanks and nothing else: "Thank you.",
/// "Thank you. Thank you.", "thank thank you".
///
/// Only meaningful where no speech was heard ([`Heard::invented`]): where there
/// was, "Thank you." is as likely said as invented.
#[must_use]
pub fn is_silence_phrase(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return false;
    }
    let thanks = words.iter().any(|w| w.starts_with("thank"))
        && words.iter().all(|w| THANKS_WORDS.contains(w));
    thanks || SILENCE_PHRASES.contains(&words.join(" ").as_str())
}

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

/// Words per second below which a turn is not somebody talking: conversation
/// runs about 2 w/s, and below this the typical turn is a few words spread over
/// half a minute of near-silence.
///
/// Four signals that look right and are refuted on this archive, so nobody
/// reaches for them again: ASR confidence (the commonest low-confidence turns
/// are the household's quiet agreement, `Ja.`, `Yeah.`, `Okay.`); tokens per
/// speech-second (a phone's suppression gates between words, so the detector
/// under-reports and the rate measures AGC, not hallucination); repetition
/// loops as a proxy for slowness (the slow band is not repetitive); and the
/// language label (most turns labelled a foreign language are Dutch and
/// English mislabelled; script outranks the label). When scoring any of this,
/// read the median: one hallucination loop moves a mean by an order of
/// magnitude.
pub const SLOW_RATE: f64 = 0.2;

/// The turn's own speaking rate: words over first-word-start to last-word-end,
/// or `None` when there is nothing to divide by.
///
/// The denominator is the turn's own span, which is device-independent. A rate
/// from a span under half a second is an artefact of the denominator, not
/// speech, so a rule on the fast side would need a minimum span;
/// [`is_implausibly_slow`] cannot be reached by a short span and needs none.
#[must_use]
pub fn speaking_rate(words: &[(f64, f64)]) -> Option<f64> {
    let first = words.first()?.0;
    let last = words.last()?.1;
    let span = last - first;
    (span > 0.0).then(|| words.len() as f64 / span)
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

/// The slow tail: a turn spoken too slowly to be somebody talking. Zeroes
/// `asr_confidence`, never deletes; the turn stays searchable and lands in the
/// review queue. The fast tail is real and too small for a rule.
#[must_use]
pub fn is_implausibly_slow(words: &[(f64, f64)]) -> bool {
    speaking_rate(words).is_some_and(|rate| rate < SLOW_RATE)
}

/// Is this character a letter Unicode names LATIN? Binary search over the
/// generated table, which covers Latin Extended and the fullwidth forms.
#[must_use]
pub fn is_latin_letter(c: char) -> bool {
    let cp = c as u32;
    crate::latin_ranges::LATIN_RANGES
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// What fraction of this turn's letters are not Latin. Only letters vote:
/// punctuation, digits and spaces say nothing about script, and a turn with no
/// letters scores 0.0.
#[must_use]
pub fn foreign_script_ratio(text: &str) -> f64 {
    let letters = text.chars().filter(|c| c.is_alphabetic());
    let (mut total, mut foreign) = (0u32, 0u32);
    for c in letters {
        total += 1;
        if !is_latin_letter(c) {
            foreign += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    f64::from(foreign) / f64::from(total)
}

/// Above this fraction of non-Latin letters, a turn is written in a script this
/// household does not speak. A majority, not a trace: one borrowed word must
/// not condemn a Dutch sentence, and the ratio is bimodal over the archive, so
/// the exact cut matters little.
pub const FOREIGN_SCRIPT_MAX: f64 = 0.5;

/// True if this turn is mostly not in Latin script. A signal, not a verdict: a
/// visitor really speaking Russian reads the same as the model contradicting
/// its own `nl` label, and what to do with a flagged turn is the caller's.
#[must_use]
pub fn is_foreign_script(text: &str) -> bool {
    foreign_script_ratio(text) > FOREIGN_SCRIPT_MAX
}
