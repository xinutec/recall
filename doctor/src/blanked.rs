//! Does every segment that ever had turns still show at least one?
//!
//! A segment with only hidden turns and no visible replacement is an impossible
//! state under refine's contract ("a refine replaces a transcript or keeps it,
//! never empties one") — yet 175 segments sat that way for weeks in July until a
//! human noticed a minute of Dutch missing. This is the day-one detector for
//! that class: every count here is a stretch of household memory currently
//! invisible, and `recall repair` puts the newest hidden generation back.
//!
//! ⚠ **The detector gates it.** Not every provenance hide was a bug: sometimes a
//! later pass was correctly dropping a hallucination, and restoring that
//! resurrects garbage. On this archive 12 of 170 restorations did exactly that —
//! "E aí", "т т т т", repeated glyphs on -64 dB silence — and they then blocked
//! the cleanup by making an empty minute look transcribed. A segment the VAD
//! heard nothing in gets nothing back, and a generation that is entirely junk is
//! not counted as restorable.

/// Reasons a turn was hidden on the EVIDENCE of what it was, rather than by a
/// pass replacing it. Such a turn is never a candidate for restoring.
///
/// ⚠ These are the sentences `recall.cleanup` writes, verbatim — not slugs.
/// Guessed slug spellings matched nothing, and a reason that never matches
/// makes every evidence-hidden turn look like a restorable generation: against
/// the real archive that reported 11 blanked segments where there are none.
const EVIDENCE_REASONS: [&str; 4] = [
    "no speech detected (VAD)",
    "repetition loop",
    "non-Latin script, no speech (VAD)",
    "no words",
];

/// Characters that carry no word. A turn made only of these says nothing about
/// what was spoken, which is why hiding one cannot lose information. The unicode
/// dashes and ellipsis are named rather than written: Whisper really does emit
/// them, and spelled literally they are indistinguishable from ASCII to a reader.
const WORDLESS: &str = ". !?*-_,:;\"'()[]{}~/\\|@#$%^&+=<>`\t\n\u{2026}\u{00b7}\u{2013}\u{2014}";

/// True if `text` contains no word at all — "...", "***", "!".
pub fn is_wordless(text: &str) -> bool {
    text.trim_matches(|c| WORDLESS.contains(c))
        .trim()
        .is_empty()
}

// A word repeated 3+ times in a row is a loop *only* if it is long enough —
// short words are real emphasis ("no no no", "who who who"), long ones are
// hallucinations ("everything everything everything").
const RUN_MIN: usize = 3;
const RUN_WORD_MIN_LEN: usize = 6;
const WORD_MIN: usize = 6; // need a few words before a dominant one means "loop"
const WORD_FRACTION: f64 = 0.5; // one token is >= half the words
const MAX_PHRASE_WORDS: usize = 6; // a repeated "phrase" up to this many words
const MIN_PHRASE_REPEATS: usize = 3; // repeated at least this many times
// A 2-8 char unit repeated 4+ times in a row ("ASTASTASTAST", "obaobaoba").
const CHAR_LOOP_UNIT: std::ops::RangeInclusive<usize> = 2..=8;
const CHAR_LOOP_REPEATS: usize = 4;
const CHAR_LOOP_MIN_LEN: usize = 12;

fn words(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut out = Vec::new();
    let mut current = String::new();
    for c in lower.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The longest run of one word repeated back-to-back, and that word.
fn longest_consecutive_run(words: &[String]) -> (usize, &str) {
    let mut best = 0;
    let mut best_word = "";
    let mut run = 0;
    let mut prev = "";
    for word in words {
        run = if word == prev { run + 1 } else { 1 };
        prev = word;
        if run > best {
            best = run;
            best_word = word;
        }
    }
    (best, best_word)
}

fn is_word_loop(text: &str) -> bool {
    let words = words(text);
    if words.is_empty() {
        return false;
    }
    // A long word repeated back-to-back — catches short hallucinated loops the
    // word-count floor below would miss.
    let (run, run_word) = longest_consecutive_run(&words);
    if run >= RUN_MIN && run_word.chars().count() >= RUN_WORD_MIN_LEN {
        return true;
    }
    if words.len() < WORD_MIN {
        return false;
    }
    // One token dominates ("momentum momentum momentum…").
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for word in &words {
        *counts.entry(word.as_str()).or_default() += 1;
    }
    if counts.values().copied().max().unwrap_or(0) as f64 / words.len() as f64 >= WORD_FRACTION {
        return true;
    }
    // A short phrase repeated back-to-back ("see you on the phone" x3).
    (1..=MAX_PHRASE_WORDS).any(|period| {
        let reps = words.len() / period;
        reps >= MIN_PHRASE_REPEATS && (0..period * reps).all(|i| words[i] == words[i % period])
    })
}

/// Space-less loops ("ASTASTAST", "obaobaoba"): a short unit repeated in a row.
///
/// The Python is a backreferenced regex (`(.{2,8}?)\1{3,}`), which the Rust
/// regex engine cannot express. Written out rather than pulling in a
/// backtracking engine for one predicate: scan left to right, and at each
/// position try the SHORTEST unit first — that is what the non-greedy `?` does,
/// and it decides which match is found, hence its length against the floor.
fn is_char_loop(text: &str) -> bool {
    let compact: Vec<char> = text
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    for start in 0..compact.len() {
        for unit in CHAR_LOOP_UNIT {
            if start + unit * CHAR_LOOP_REPEATS > compact.len() {
                continue;
            }
            let mut reps = 1;
            while start + unit * (reps + 1) <= compact.len()
                && (0..unit).all(|i| compact[start + i] == compact[start + reps * unit + i])
            {
                reps += 1; // `\1{3,}` is greedy: take as many as there are
            }
            if reps >= CHAR_LOOP_REPEATS && unit * reps >= CHAR_LOOP_MIN_LEN {
                return true;
            }
        }
    }
    false
}

/// True if `text` is a degenerate repetition loop (a model artifact).
pub fn is_repetition_loop(text: &str) -> bool {
    is_word_loop(text) || is_char_loop(text)
}

/// One hidden turn, as `segments_showing_no_turns` returns it.
#[derive(Debug, Clone)]
pub struct HiddenTurn {
    pub id: i64,
    pub hidden_reason: String,
    pub text: String,
}

/// The newest generation: the trailing run, in id order, hidden by a single
/// pass.
///
/// Each pass hides the generation before it, so a segment accretes them —
/// original, reprocessed, diarized — and the *last* run sharing one hidden
/// reason is the newest, and best, transcript that ever existed for it. A turn
/// hidden on the evidence of what it was (a hallucination, a repetition loop) is
/// never restored.
pub fn last_generation(turns: &[HiddenTurn]) -> Vec<i64> {
    let generations: Vec<&HiddenTurn> = turns
        .iter()
        .filter(|t| !EVIDENCE_REASONS.contains(&t.hidden_reason.as_str()))
        .collect();
    let Some(newest) = generations.last() else {
        return Vec::new();
    };
    let newest_reason = newest.hidden_reason.as_str();
    let mut run: Vec<i64> = generations
        .iter()
        .rev()
        .take_while(|t| t.hidden_reason == newest_reason)
        .map(|t| t.id)
        .collect();
    run.reverse();
    run
}

/// The subset worth bringing back: whatever cleanup would not hide on sight.
///
/// Pure text only — the foreign-script rule needs audio, and the caller has
/// already excluded segments the detector heard nothing in, so script over real
/// speech is protected there rather than here.
pub fn any_restorable(texts: &[&str]) -> bool {
    texts
        .iter()
        .any(|t| !is_wordless(t) && !is_repetition_loop(t))
}
