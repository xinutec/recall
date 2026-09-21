//! The text rules, against the Python they were ported from and against the
//! shapes a reader needs to see spelled out.
//!
//! ⚠ **`recall.quality` AND ITS GENERATOR ARE DELETED** (2026-09-18), so the
//! corpus below is a frozen record of the Python's behaviour rather than a live
//! comparison, and it can never be regenerated. If a case in it ever looks
//! wrong, the question is whether the RUST is right — there is nothing left to
//! ask.
//!
//! ⚠ The fixture's verdicts were produced BY the Python (`scripts/gen_quality_parity.py`,
//! seed 20260912), so they pin the parts no hand-written case reaches: which
//! match `re.search` returns for the character loop, where `\w` splits a word
//! carrying a diaeresis, and what `str.strip(chars)` does to a turn that is
//! punctuation at one end only.
//!
//! ⚠ **The corpus is GENERATED, not sampled from the archive, and that is a real
//! limitation rather than a preference.** This repository is public and the
//! archive is a household's conversations, so the texts are nonsense tokens in
//! the shapes the archive contains. What it cannot prove is that the rules agree
//! with the Python on REAL rows; the differential over the live archive is the
//! evidence for that, and it belongs in a task rather than a committed file.
//!
//! Every threshold in `text.rs` was moved by one and this was checked to go red
//! — twice it did not, and both times the corpus was at fault rather than the
//! code: there was no five-letter token, so `RUN_WORD_MIN_LEN`'s split was
//! unpinned, and the six-word phrases were drawn at random, so a repeated word
//! let the run rule answer before the phrase rule could. One threshold resists
//! the technique for a real reason: a character loop's match is always
//! `repeats × unit` with repeats ≥ 4 and unit ≤ 8, so no match can be 11
//! characters long and `CHAR_LOOP_MIN_LEN` = 11 is the SAME PROGRAM as 12. The
//! ablation that pins it is 12 → 10.

use audiocore::text::{is_repetition_loop, is_wordless};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    text: String,
    loop_: bool,
    wordless: bool,
}

// `loop` is a keyword, and the fixture is the Python's field name.
impl Case {
    fn parse(raw: &str) -> Vec<Self> {
        #[derive(Deserialize)]
        struct Wire {
            text: String,
            #[serde(rename = "loop")]
            loop_: bool,
            wordless: bool,
        }
        serde_json::from_str::<Vec<Wire>>(raw)
            .expect("fixture")
            .into_iter()
            .map(|w| Case {
                text: w.text,
                loop_: w.loop_,
                wordless: w.wordless,
            })
            .collect()
    }
}

#[test]
fn the_rust_text_rules_match_the_python_ones_case_for_case() {
    let cases = Case::parse(include_str!("fixtures/quality-parity.json"));
    assert!(cases.len() >= 400, "the corpus must not silently shrink");
    // A corpus that is all one verdict would pass a port that always answers
    // that verdict, which is the failure mode a parity test exists to catch.
    let loops = cases.iter().filter(|c| c.loop_).count();
    let wordless = cases.iter().filter(|c| c.wordless).count();
    assert!(loops > 20 && loops < cases.len() - 20, "loops: {loops}");
    assert!(wordless > 0, "wordless: {wordless}");

    for (index, case) in cases.iter().enumerate() {
        assert_eq!(
            is_repetition_loop(&case.text),
            case.loop_,
            "case {index}: is_repetition_loop({:?})",
            case.text
        );
        assert_eq!(
            is_wordless(&case.text),
            case.wordless,
            "case {index}: is_wordless({:?})",
            case.text
        );
    }
}

#[test]
fn the_string_the_two_copies_once_disagreed_about() {
    // ⚠ Enough repeats of a SHORT unit to qualify, spanning under the minimum,
    // followed by a genuine long loop. Searching on past the first match — the
    // natural reading of a greedy `\1{3,}`, and what the doctor's copy did —
    // answers "loop" here, where the Python returns. That disagreement meant a
    // turn recalld had written read as unrestorable junk.
    assert!(!is_repetition_loop("ababababxyzxyzxyzxyzxyz"));
}

#[test]
fn punctuation_only_turns_are_wordless() {
    assert!(is_wordless("..."));
    assert!(is_wordless(" *** "));
    assert!(is_wordless("!"));
    assert!(is_wordless("\u{2026}"));
    assert!(is_wordless(""));
    assert!(!is_wordless("Ja."));
    // Dutch is half this archive: an accented word is a word.
    assert!(!is_wordless("héél"));
}

#[test]
fn a_long_word_repeated_three_times_is_a_loop_and_a_short_one_is_emphasis() {
    assert!(is_repetition_loop("everything everything everything"));
    // Real speech. Hiding this would delete somebody actually saying it.
    assert!(!is_repetition_loop("no no no"));
    assert!(!is_repetition_loop("who who who"));
}

#[test]
fn a_dominant_token_over_enough_words_is_a_loop() {
    assert!(is_repetition_loop(
        "momentum a momentum b momentum c momentum"
    ));
}

#[test]
fn a_repeated_phrase_is_a_loop() {
    assert!(is_repetition_loop(
        "see you on the phone see you on the phone see you on the phone"
    ));
}

#[test]
fn a_spaceless_unit_repeated_four_times_is_a_loop() {
    assert!(is_repetition_loop("ASTASTASTAST"));
    assert!(is_repetition_loop("obaobaobaoba"));
    // Three repeats of a 2-char unit is only 6 characters — under the floor,
    // and the floor is what keeps "haha" and "bye bye" out of it.
    assert!(!is_repetition_loop("hahaha"));
}

#[test]
fn ordinary_speech_is_not_a_loop() {
    assert!(!is_repetition_loop("Ik denk dat we morgen gaan."));
    assert!(!is_repetition_loop("Shall we have dinner at seven?"));
    assert!(!is_repetition_loop(""));
}
