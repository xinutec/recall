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

/// True if `text` is nothing but one of `names` — "Anna.", " anna ", "Anna!".
///
/// ⚠ **This is the cost of the vocabulary prompt, not a hallucination rule in
/// general.** The ASR prompt lists household names FIRST so Whisper spells them
/// right, so on audio it cannot place, it reaches for them: measured over every
/// short turn ever written, the live tier is 7x more likely than the archive
/// pass to emit a turn that is nothing but a name (#1665). It was caught with
/// spoken ground truth — eight scripted lines containing no names produced a
/// household first name four times.
///
/// ⚠ **Why this one is refused rather than kept without confidence**, which is
/// what a non-household language or a foreign script gets: a turn that is only
/// a name carries nothing a memory aid needs, and what it DOES carry is the
/// assertion that a specific person spoke. Installing a false memory is the
/// harm this system exists to guard against, and it is invisible to every other
/// signal — fluent, Latin script, correctly language-labelled, plausibly timed.
///
/// ⚠ It costs the real vocative: somebody calling "Anna!" across the room is
/// refused too. That is affordable HERE and nowhere else, because the tier this
/// runs on is provisional — the archive pass re-derives the same minute from a
/// 60 s clip with the context to tell the two apart, and supersedes whatever
/// this wrote.
#[must_use]
pub fn is_bare_name(text: &str, names: &[String]) -> bool {
    let bare = text.trim_matches(|c| WORDLESS.contains(c)).trim();
    !bare.is_empty()
        && names
            .iter()
            .any(|name| name.trim().eq_ignore_ascii_case(bare))
}

/// ⚠⚠ **FOUR SIGNALS THAT LOOKED RIGHT AND ARE REFUTED.** Every one was measured
/// on this archive and every one would be reached for again by anybody trying to
/// find junk in a transcript. They are here rather than in a task because this
/// is the file where the next attempt will be written.
///
/// * **ASR confidence.** The commonest low-confidence turns in this archive ARE
///   the quiet agreement a memory aid must keep: `Ja.` 421 times, `Yeah.` 108,
///   `Okay.` 55. Filtering on it deletes the household agreeing with each other.
/// * **Tokens per speech-second.** Of 32 turns at >=20 words per speech-second,
///   25 had a concurrent microphone and ALL 25 of those measured speech — it
///   fires when the room really was talking. The denominator is wrong PER
///   DEVICE: a phone's suppression gates between words, so silero reports a
///   fraction of the speech present while Whisper still transcribes the
///   fragments. It measures AGC aggression, not hallucination.
/// * **Repetition loops, as a proxy for this band.** These are not loops — the
///   median distinct-word ratio in the slow band is 0.641, HIGHER than the
///   normal band's 0.590. ⓘ Real loops are a separate rule and it works;
///   see [`is_repetition_loop`].
/// * **The language LABEL.** 681 visible turns are labelled es/de/pt/tr but only
///   180 are in a non-Latin SCRIPT; the rest are Dutch and English the model
///   mislabelled, so the label alone would zero real speech. ⚠ Script outranks
///   the label, never the reverse.
///
/// ⚠ **And read the MEDIAN, never the mean, when scoring any of this.** Two runs
/// over the same unchanged audio scored mean WER 0.666 and 16.449 while the
/// median was 0.229 both times — four short utterances had come back as
/// hallucination loops ("Yep." against "As to As to As to…" several hundred
/// times, WER 223).
///
/// Words per second below which a turn is not somebody talking. Measured on the
/// 4,989 turns carrying usable timings: the median is 2.18 w/s — human
/// conversational speed — and below 0.2 the median turn is **four words spread
/// over 32 seconds**, which is a single word over near-silence.
pub const SLOW_RATE: f64 = 0.2;

/// The turn's own speaking rate: words over first-word-start to last-word-end.
///
/// ⚠ **The denominator is the turn's OWN span, and that is the whole point.**
/// Every earlier junk signal reached OUTSIDE the turn for a denominator and a
/// per-DEVICE one is what broke them — tokens per speech-second fires on
/// minutes when the room really was talking, because a phone's suppression
/// gates between words, so silero reports a fraction of the speech present
/// while Whisper still transcribes the fragments. It measured AGC aggression,
/// not hallucination. Word timings are device-independent.
///
/// `None` when there is nothing to divide by.
///
/// ⚠⚠ **READ THE DENOMINATOR BEFORE BELIEVING A RATE FROM THIS.** Turns carrying
/// the shim's verbatim timings were once read as giving p95 30 w/s and max 550,
/// and that shape was set aside as not meaning what this assumes. It means
/// exactly what it says; the spread is the denominator's:
///
/// ```text
/// span < 0.5 s    6,652 turns   mean 22.6 w/s   max 600.0   ⛔
/// span 0.5-2 s    9,417 turns   mean  2.5 w/s   max  22.0
/// span 2-10 s     5,669 turns   mean  2.1 w/s   max  29.0
/// span >= 10 s      879 turns   mean  1.6 w/s   max  17.8
/// ```
///
/// **Every impossible rate is a sub-half-second span**, and above that the two
/// encodings read alike. ⓘ [`is_implausibly_slow`] is immune by construction —
/// 0.2 w/s takes five seconds per word, so no short span can reach it — which is
/// why there is no minimum-span guard here. A rule on the FAST side would need
/// one, and would be building on the artefact without it.
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

/// Words with their text and timing, out of a stored `word_timings` value.
///
/// ⚠ **The TEXT matters as much as the span** for anything that divides a turn:
/// a split is only safe if the pieces can be shown to carry every word the
/// original did, and that check needs the words themselves.
///
/// See [`word_spans`] for the two encodings this reads.
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

/// Word spans out of a stored `word_timings` value, whichever way it is
/// spelled.
///
/// ⚠ **Two encodings are stored and they are both honest.** `diarized` writes
/// recalld's own `{s,e,w}`, re-based to the turn; `turns` stores the ASR shim's
/// reply VERBATIM as `{start,end,probability,text}`, absolute within the clip.
/// Reading only one of them is what limited this signal to a tenth of the
/// archive. ⚠ The absolute one must never be compared ACROSS turns — only its
/// own first-to-last span is meaningful here.
///
/// ⓘ Reads through [`timed_words`], so the two cannot drift apart about which
/// spellings exist.
#[must_use]
pub fn word_spans(timings: &str) -> Vec<(f64, f64)> {
    timed_words(timings)
        .into_iter()
        .map(|w| (w.start, w.end))
        .collect()
}

/// The slow tail, corroborated twice by instruments sharing nothing with word
/// timings: foreign script runs 3.0% in this band against 0.3% in the normal
/// one, and on the only microphone whose VAD readings are trustworthy the band
/// carries a median 0.9 s of speech against 31.2 s.
///
/// ⚠ **Only ever `asr_confidence = 0.0`, never deletion** — the same treatment a
/// foreign script gets. The turn is kept, stays searchable, and lands in the
/// review queue instead of heading a conversation card.
///
/// ⚠ The FAST tail is real and negligible — 17 turns above 10 w/s across the
/// whole corpus — so it gets no rule. A rule for 17 turns is a rule whose false
/// positives outnumber its finds.
///
/// ⓘ Either stored encoding may be fed to this — see [`word_spans`]. What must
/// NOT be done is compare one turn's absolute timings against another's.
#[must_use]
pub fn is_implausibly_slow(words: &[(f64, f64)]) -> bool {
    speaking_rate(words).is_some_and(|rate| rate < SLOW_RATE)
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
