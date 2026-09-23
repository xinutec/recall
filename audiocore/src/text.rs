//! Does this transcript text say anything, or is it a model artifact? recalld
//! asks before writing a turn.
//!
//! `tests/text.rs` pins the behaviour case by case; it is the specification.

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
/// no"); long ones are hallucinations ("everything everything everything").
/// Six is calibrated from the archive.
const RUN_WORD_MIN_LEN: usize = 6;
/// A space-less loop's repeated unit is 2-8 characters ("ASTASTASTAST").
const CHAR_UNIT_MIN: usize = 2;
const CHAR_UNIT_MAX: usize = 8;
/// …repeated this many times in a row, total…
const CHAR_LOOP_MIN_REPEATS: usize = 4;
/// …spanning at least this many characters. Shorter runs are ordinary words.
const CHAR_LOOP_MIN_LEN: usize = 12;

/// Characters that carry no word; a turn made only of these says nothing. The
/// unicode dashes and ellipsis (which Whisper emits) are escapes so a reader
/// can tell them from ASCII.
const WORDLESS: &str = ". !?*-_,:;\"'()[]{}~/\\|@#$%^&+=<>`\t\n\u{2026}\u{00b7}\u{2013}\u{2014}";

/// `text` with everything that carries no word stripped from both ends.
///
/// Exposed so a caller asking "is this turn nothing but X" strips exactly as
/// [`is_wordless`] does.
#[must_use]
pub fn trim_wordless(text: &str) -> &str {
    text.trim_matches(|c| WORDLESS.contains(c)).trim()
}

/// True if `text` contains no word at all: "...", "***", "!". A single signal
/// suffices: the question is whether the text says anything, not whether it
/// was speech.
#[must_use]
pub fn is_wordless(text: &str) -> bool {
    trim_wordless(text).is_empty()
}

/// True if `text` is a degenerate repetition loop (a model artifact).
#[must_use]
pub fn is_repetition_loop(text: &str) -> bool {
    is_word_loop(text) || is_char_loop(text)
}

/// The lower-cased word tokens: runs of Unicode alphanumerics and underscore.
///
/// ⚠ Unicode, not `[A-Za-z0-9_]`: ASCII would split "coördinatie" into two
/// tokens and hide a two-word loop.
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
    // A long word repeated back-to-back. Before the word-count floor on purpose:
    // "everything everything everything" is only three words.
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
/// ⚠ The first such run decides, not the longest: the leftmost, shortest-unit
/// match of `(.{2,8}?)\1{3,}`, with the length test applied to that match. So a
/// four-fold "abab" early in a turn answers "not a loop" even when a longer run
/// follows; searching on answers differently on real rows, and a test pins the
/// string that separates them.
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
