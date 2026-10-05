//! Whether transcript text is trustworthy, decided from a line's own text and
//! timings. Used by `render`; re-exported as `recalld::quality`.

/// Measured speech under which a minute counts as near-silent, in seconds. The
/// speech pass's floor is one 0.256 s blip; in minutes under a second the
/// commonest lines were "Thank you." and video sign-offs (#1461).
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

/// True if `text` is a phrase the model writes over silence, or thanks and
/// nothing else ("Thank you. Thank you.", "thank thank you").
///
/// Only meaningful where no speech was heard: where there was, "Thank you." is
/// as likely said as invented.
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
/// Signals refuted on this archive as hallucination detectors:
/// - ASR confidence: the commonest low-confidence turns are quiet agreement
///   (`Ja.`, `Yeah.`, `Okay.`).
/// - Tokens per speech-second: a phone gates between words, so the detector
///   under-reports and the rate measures AGC.
/// - Repetition loops: the slow band is not repetitive.
/// - The language label: most turns labelled foreign are mislabelled Dutch or
///   English; script outranks it.
///
/// Score with the median: one loop moves a mean by an order of magnitude.
pub const SLOW_RATE: f64 = 0.2;

/// Words per second from the first word's start to the last word's end; `None`
/// for a zero span.
///
/// A span under half a second gives a meaningless high rate, so a fast-side
/// rule would need a minimum span; the slow-side rule needs none.
#[must_use]
pub fn speaking_rate(words: &[(f64, f64)]) -> Option<f64> {
    let first = words.first()?.0;
    let last = words.last()?.1;
    let span = last - first;
    (span > 0.0).then(|| words.len() as f64 / span)
}

/// A turn spoken too slowly to be somebody talking. Callers zero its
/// confidence rather than drop it. The fast tail is real and too small for a
/// rule.
#[must_use]
pub fn is_implausibly_slow(words: &[(f64, f64)]) -> bool {
    speaking_rate(words).is_some_and(|rate| rate < SLOW_RATE)
}

/// Whether Unicode names this character Latin, including Latin Extended and
/// the fullwidth forms.
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

/// The fraction of the turn's letters that are not Latin; 0.0 with no letters.
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

/// Above this fraction of non-Latin letters, a turn is in a script the
/// household does not speak. A majority, so one borrowed word does not flag a
/// Dutch sentence; the ratio is bimodal, so the exact cut matters little.
pub const FOREIGN_SCRIPT_MAX: f64 = 0.5;

/// True if the turn is mostly not Latin script. A signal, not a verdict: a
/// visitor speaking Russian looks the same as an invention.
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
