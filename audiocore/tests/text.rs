//! The text rules, against a frozen corpus and the shapes a reader needs spelled out.
//!
//! The corpus holds verdicts from the implementation these rules replaced and
//! cannot be regenerated; if a case looks wrong, judge the Rust on its merits.
//! It pins edge cases hand-written examples miss: which match the character
//! loop finds, where a word with a diaeresis splits, and stripping a turn that
//! is punctuation at one end only.
//!
//! The texts are generated nonsense tokens in the archive's shapes, since the
//! repository is public; they cannot show agreement on real rows.
//!
//! Moving any threshold in `text.rs` by one turns the corpus red, with one
//! exception: a character loop's match is `repeats × unit` with repeats ≥ 4
//! and unit ≤ 8, so no match is 11 characters long and `CHAR_LOOP_MIN_LEN` of
//! 11 behaves as 12.

use audiocore::text::{is_repetition_loop, is_wordless};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    text: String,
    loop_: bool,
    wordless: bool,
}

// `loop` is a keyword, and it is the fixture's field name.
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
    // A corpus of one verdict would pass rules that always answer it.
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
    // Enough repeats of a short unit to qualify but spanning under the minimum,
    // followed by a genuine long loop. Only the first match is judged, so
    // searching on past it would answer "loop" here and disagree with the corpus.
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
    // An accented word is a word; much of the archive is Dutch.
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
    // Three repeats of a 2-char unit is only 6 characters, under the floor
    // that keeps laughter like "hahaha" out.
    assert!(!is_repetition_loop("hahaha"));
}

#[test]
fn ordinary_speech_is_not_a_loop() {
    assert!(!is_repetition_loop("Ik denk dat we morgen gaan."));
    assert!(!is_repetition_loop("Shall we have dinner at seven?"));
    assert!(!is_repetition_loop(""));
}
