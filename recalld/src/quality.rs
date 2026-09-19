//! Is this transcript text trustworthy? — the Rust half of `recall.quality`.
//!
//! Whisper loops on hard or short audio ("goog goog goog…", "ASTASTAST…") and
//! emits punctuation where it heard nothing ("...", "***"). No human utterance
//! does either, so dropping them loses nothing — and unlike every other quality
//! signal tried here, both are decidable from the TEXT alone: no audio decode,
//! no VAD, no per-device calibration. That is why these two ported and the
//! others did not (#1410 refuted tokens-per-speech-second; the language LABEL
//! was never safe — 681 turns labelled es/de/pt/tr are Dutch and English the
//! model merely mislabelled).
//!
//! The only implementation: `recall.quality` was deleted, so the parity corpus
//! is a frozen record of the port rather than a live comparison.
//!
//! Both rules run at write time (`turns.rs`, `work.rs`), so a looping or
//! wordless turn never reaches the read path.
//!
//! Not ported: `foreign_script_ratio`. It asks whether a letter's Unicode NAME
//! contains "LATIN", which has no dependency-free Rust equivalent that agrees
//! with Python on the edges (ª, µ and the combining marks are alphabetic with
//! no LATIN in their names). The archive's non-Latin residue is ~180 turns and
//! it is not what the room stream gets wrong — it reports a Dutch household in
//! English, which is Latin script and passes any such filter. Port it when
//! there is a reason, and port it through a GENERATED range table rather than a
//! guess at the block boundaries.

/// Need a few words before a dominant one means "loop".
const WORD_MIN: usize = 6;
/// One token is at least half the words.
const WORD_FRACTION: f64 = 0.5;
/// A repeated "phrase" up to this many words.
const MAX_PHRASE_WORDS: usize = 6;
/// Repeated at least this many times.
const MIN_PHRASE_REPEATS: usize = 3;
/// A word repeated back-to-back this many times is a candidate loop.
const RUN_MIN: usize = 3;
/// …but only if the word is this long. Short words are real emphasis ("no no
/// no", "who who who"); long ones are hallucinations ("everything everything
/// everything"). Six is calibrated from the archive, not chosen.
const RUN_WORD_MIN_LEN: usize = 6;
/// A space-less loop's repeated unit is 2-8 characters ("ASTASTASTAST").
const CHAR_UNIT_MIN: usize = 2;
const CHAR_UNIT_MAX: usize = 8;
/// …repeated this many times in a row, total…
const CHAR_LOOP_MIN_REPEATS: usize = 4;
/// …spanning at least this many characters. Shorter runs are ordinary words.
const CHAR_LOOP_MIN_LEN: usize = 12;

/// Characters that carry no word. A turn made only of these says nothing about
/// what was spoken, which is why hiding one cannot lose information. The unicode
/// dashes and ellipsis are written as escapes: Whisper really does emit them,
/// and spelled literally they are indistinguishable from ASCII to a reader.
const WORDLESS: &str = ". !?*-_,:;\"'()[]{}~/\\|@#$%^&+=<>`\t\n\u{2026}\u{00b7}\u{2013}\u{2014}";

/// True if `text` contains no word at all — "...", "***", "!".
///
/// The only single-signal rule here, and it needs no second one: the test is not
/// "was this probably speech" but "does this text say anything", and the answer
/// is no however loud the room was.
#[must_use]
pub fn is_wordless(text: &str) -> bool {
    text.trim_matches(|c| WORDLESS.contains(c))
        .trim()
        .is_empty()
}

/// Is this character a letter Python's `unicodedata` names LATIN?
///
/// Binary search over the generated table, which is why it agrees with the
/// Python on Latin Extended and the fullwidth forms rather than guessing at
/// block boundaries.
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

/// What fraction of this turn's LETTERS are not Latin.
///
/// ⚠ **Only letters vote.** Punctuation, digits and spaces are not evidence
/// about script, and counting them would read "..." as wholly foreign — a case
/// [`is_wordless`] already owns. A turn with no letters scores 0.0: nothing was
/// written in any script.
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
/// household does not speak.
///
/// ⚠ **A MAJORITY, not a trace.** One borrowed word must not condemn a Dutch
/// sentence. Measured 2026-09-19 over the whole archive: the ratio is strongly
/// bimodal — 818 of 1,301 multi-byte turns sit at 0.0 and 305 at 1.0 — so the
/// exact cut matters far less than being on the right side of the gap.
pub const FOREIGN_SCRIPT_MAX: f64 = 0.5;

/// True if this turn is mostly not in Latin script.
///
/// ⚠ **A SIGNAL, not a verdict.** The household speaks Dutch and English, so
/// Cyrillic or Japanese in a turn the model itself labelled `nl` is the model
/// contradicting itself — but a person really speaking Russian would read the
/// same, and this cannot tell those apart. Deciding what to DO with a flagged
/// turn is not this function's business (#1410).
#[must_use]
pub fn is_foreign_script(text: &str) -> bool {
    foreign_script_ratio(text) > FOREIGN_SCRIPT_MAX
}

/// True if `text` is a degenerate repetition loop (a model artifact).
#[must_use]
pub fn is_repetition_loop(text: &str) -> bool {
    is_word_loop(text) || is_char_loop(text)
}

/// The word tokens Python's `re.findall(r"\w+", text.lower())` finds.
///
/// ⚠ `\w` on a Python `str` is Unicode-aware — alphanumerics plus underscore —
/// not `[A-Za-z0-9_]`. Reading it as ASCII would split every Dutch word carrying
/// a diaeresis into two tokens and turn "coördinatie coördinatie" into a
/// four-token run that looks nothing like the two-token loop it is.
fn words(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    lowered
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The longest run of one word repeated back-to-back, and that word.
fn longest_consecutive_run(words: &[String]) -> (usize, &str) {
    let (mut best, mut best_word, mut run, mut prev) = (0, "", 0, "");
    for word in words {
        run = if word == prev { run + 1 } else { 1 };
        if run > best {
            best = run;
            best_word = word;
        }
        prev = word;
    }
    (best, best_word)
}

fn is_word_loop(text: &str) -> bool {
    let words = words(text);
    if words.is_empty() {
        return false;
    }
    // A long word repeated back-to-back. This runs BEFORE the word-count floor
    // on purpose: "everything everything everything" is three words and would
    // otherwise never be examined.
    let (run, run_word) = longest_consecutive_run(&words);
    if run >= RUN_MIN && run_word.chars().count() >= RUN_WORD_MIN_LEN {
        return true;
    }
    if words.len() < WORD_MIN {
        return false;
    }
    // One token dominates ("momentum momentum momentum…").
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for word in &words {
        *counts.entry(word.as_str()).or_default() += 1;
    }
    let most = counts.values().copied().max().unwrap_or(0);
    if most as f64 / words.len() as f64 >= WORD_FRACTION {
        return true;
    }
    // A short phrase repeated back-to-back ("see you on the phone" x3).
    for period in 1..=MAX_PHRASE_WORDS {
        let reps = words.len() / period;
        if reps < MIN_PHRASE_REPEATS {
            continue;
        }
        let head = &words[..period];
        if (0..reps).all(|r| &words[r * period..(r + 1) * period] == head) {
            return true;
        }
    }
    false
}

/// Space-less loops ("ASTASTAST", "obaobaoba"): a short unit repeated in a row.
///
/// ⚠ **The FIRST such run decides, not the longest**, and that is the Python's
/// behaviour rather than an oversight worth fixing here. `re.search` of
/// `(.{2,8}?)\1{3,}` returns the leftmost match with the shortest unit, and the
/// length test is applied to THAT match — so a four-fold "abab" early in a turn
/// answers "not a loop" even when a forty-character run follows it. Changing it
/// would make the two implementations disagree on real archive rows, which is
/// the one thing a port may not do quietly.
fn is_char_loop(text: &str) -> bool {
    let compact: Vec<char> = text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for start in 0..compact.len() {
        for unit in CHAR_UNIT_MIN..=CHAR_UNIT_MAX {
            if start + unit * CHAR_LOOP_MIN_REPEATS > compact.len() {
                break;
            }
            let head = &compact[start..start + unit];
            let mut repeats = 1;
            while start + (repeats + 1) * unit <= compact.len()
                && &compact[start + repeats * unit..start + (repeats + 1) * unit] == head
            {
                repeats += 1;
            }
            if repeats >= CHAR_LOOP_MIN_REPEATS {
                return repeats * unit >= CHAR_LOOP_MIN_LEN;
            }
        }
    }
    false
}
