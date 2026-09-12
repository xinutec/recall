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
//! ⚠ **This is a PORT, and `recall.quality` is still the original.** The Mac's
//! writers (live, worker, refine) apply the Python; this applies to the room
//! stream recalld writes itself. Two implementations of one rule is exactly the
//! drift `recall.quality`'s own docstring was written about, and the answer is
//! the same one: they are checked against each other over the whole archive,
//! not against shared unit tests both were written to pass. See
//! `docs/architecture.md`, stage D3.
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
