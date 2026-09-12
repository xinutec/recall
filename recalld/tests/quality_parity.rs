//! The port agrees with `recall.quality` — on 400 generated cases, not on the
//! handful of examples both implementations were written from.
//!
//! ⚠ The fixture's verdicts were produced BY the Python (`scripts/gen_quality_parity.py`,
//! seed 20260912), so they pin that implementation's behaviour at the moment of
//! the port — including the parts no shared unit test can reach: which match
//! `re.search` returns for the character loop, where `\w` splits a word carrying
//! a diaeresis, and what `str.strip(chars)` does to a turn that is punctuation at
//! one end only.
//!
//! ⚠ **The corpus is GENERATED, not sampled from the archive, and that is a real
//! limitation rather than a preference.** This repository is public and the
//! archive is a household's conversations, so the texts here are nonsense tokens
//! in the shapes the archive contains. What that cannot prove is that the two
//! agree on REAL rows; the differential over the live archive is the evidence for
//! that, and it belongs in the task rather than in a committed file.
//!
//! Every threshold in `quality.rs` was moved by one and this test was checked to
//! go red — twice it did not, and both times the corpus was at fault rather than
//! the code: there was no five-letter token, so `RUN_WORD_MIN_LEN`'s split was
//! unpinned, and the six-word phrases were drawn at random, so a repeated word
//! let the run rule answer before the phrase rule could. One threshold resists
//! the technique for a real reason: a character loop's match is always
//! `repeats × unit` with repeats ≥ 4 and unit ≤ 8, so no match can be 11
//! characters long and `CHAR_LOOP_MIN_LEN` = 11 is the SAME PROGRAM as 12. The
//! ablation that pins it is 12 → 10.
//!
//! When `recall.quality` is deleted this stops being a parity check and becomes a
//! plain regression corpus. That is the intended end state, and it is why the
//! verdicts are stored rather than computed at test time — nothing here may
//! depend on Python existing.

use recalld::quality::{is_repetition_loop, is_wordless};
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
fn the_rust_quality_rules_match_the_python_ones_case_for_case() {
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
