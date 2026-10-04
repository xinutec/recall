//! Is this transcript text trustworthy? The rules decidable from a line's own
//! text and timings, which `render` applies and recalld re-exports
//! (`recalld::quality`).

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
/// `SILENCE_PHRASES`), or thanks and nothing else: "Thank you.",
/// "Thank you. Thank you.", "thank thank you".
///
/// Only meaningful where no speech was heard (`recalld::quality::Heard::invented`): where there
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

/// Seconds of `regions` (start, end, from the clip's start) inside `[start, end)`.
#[must_use]
pub fn speech_inside(regions: &[(f64, f64)], start: f64, end: f64) -> f64 {
    regions
        .iter()
        .map(|&(a, b)| (b.min(end) - a.max(start)).max(0.0))
        .sum()
}
