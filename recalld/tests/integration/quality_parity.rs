//! The port agrees with `recall.quality` — on 400 generated cases, not on the
//! handful of examples both implementations were written from.
//!
//! ⚠ **`recall.quality` AND ITS GENERATOR ARE DELETED** (2026-09-18), so this is
//! no longer a parity check between two live halves: it is a frozen record of
//! the Python's behaviour, and the corpus can never be regenerated. That makes
//! it strictly a regression fixture — if a case here ever looks wrong, the
//! question is whether the RUST is right, because there is nothing left to ask.
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
    let cases = Case::parse(include_str!("../fixtures/quality-parity.json"));
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

// --- the turn's own speaking rate (#1410) ------------------------------------

use recalld::quality::{SLOW_RATE, is_implausibly_slow, speaking_rate, word_spans};

/// Word spans at a steady `rate`, starting at zero.
fn at_rate(words: usize, rate: f64) -> Vec<(f64, f64)> {
    let step = 1.0 / rate;
    (0..words)
        .map(|i| {
            let start = i as f64 * step;
            (start, start + step * 0.5)
        })
        .collect()
}

#[test]
fn the_slow_tail_is_a_single_word_over_near_silence_not_slow_speech() {
    // ⚠ The measured shape of the band: four words spread over 32 seconds. The
    // median of the whole corpus is 2.18 w/s — human conversational speed — so
    // a rule that caught ordinary slow talking would catch the archive.
    let junk = vec![(0.0, 0.4), (11.0, 11.3), (21.0, 21.4), (32.0, 32.4)];
    assert!(is_implausibly_slow(&junk));

    // Deliberate, careful speech is NOT this. A memory aid whose quality rule
    // fires on someone speaking slowly has misread what it is for.
    let deliberate = at_rate(12, 1.0);
    assert!(!is_implausibly_slow(&deliberate));
    assert!(!is_implausibly_slow(&at_rate(20, 2.18)));
}

#[test]
fn a_turn_with_nothing_to_divide_by_is_not_accused() {
    // ⚠ `None`, not "infinitely fast" and not "infinitely slow". A turn whose
    // words all carry the same instant says the timings are unusable, and a
    // rule that read that as a verdict would grade the encoding, not the speech.
    assert_eq!(speaking_rate(&[]), None);
    assert_eq!(speaking_rate(&[(5.0, 5.0)]), None);
    assert!(!is_implausibly_slow(&[]));
    assert!(!is_implausibly_slow(&[(5.0, 5.0)]));
}

#[test]
fn the_fast_tail_gets_no_rule_because_it_is_seventeen_turns() {
    // 17 turns above 10 w/s across the whole corpus. A rule for 17 turns is one
    // whose false positives outnumber its finds — and the physically impossible
    // rates live in the OTHER timing encoding, which this must never be fed.
    assert!(!is_implausibly_slow(&at_rate(8, 30.0)));
    assert!(speaking_rate(&at_rate(8, 30.0)).is_some_and(|r| r > 10.0));
}

#[test]
fn the_cut_is_read_from_the_rule_rather_than_copied_beside_it() {
    // ⚠ A threshold pasted into a test drifts from the one that ships, and then
    // the test certifies the number it was written against rather than the rule.
    let just_under = at_rate(6, SLOW_RATE * 0.9);
    let just_over = at_rate(6, SLOW_RATE * 1.1);
    assert!(is_implausibly_slow(&just_under));
    assert!(!is_implausibly_slow(&just_over));
}

#[test]
fn both_stored_timing_encodings_are_read() {
    // ⚠ `diarized` writes recalld's own `{s,e,w}`, re-based to the turn;
    // `turns` stores the shim's reply VERBATIM as `{start,end,probability,text}`,
    // absolute within the clip. Reading only the first is what limited this
    // signal to a tenth of the archive.
    let ours = r#"[{"s":0.0,"e":0.4,"w":"one"},{"s":11.0,"e":11.3,"w":"two"}]"#;
    let shims = r#"[{"start":32.0,"end":32.4,"probability":0.9,"text":"one"},
                    {"start":43.0,"end":43.3,"probability":0.8,"text":"two"}]"#;
    assert_eq!(word_spans(ours), vec![(0.0, 0.4), (11.0, 11.3)]);
    assert_eq!(word_spans(shims), vec![(32.0, 32.4), (43.0, 43.3)]);
    // The same junk, spelled both ways, gets the same verdict — which is the
    // whole point of reading both.
    assert!(is_implausibly_slow(&word_spans(ours)));
    assert!(is_implausibly_slow(&word_spans(shims)));
}

#[test]
fn a_turn_with_no_usable_timings_accuses_nobody() {
    // ⚠ An absent or unreadable encoding must read as "no opinion", never as a
    // verdict: a rule that graded the ENCODING would zero confidence on every
    // turn whose shape it had not been taught.
    for timings in ["", "not json", "{}", "[]", r#"[{"probability":0.9}]"#] {
        assert!(word_spans(timings).is_empty(), "{timings:?}");
        assert!(!is_implausibly_slow(&word_spans(timings)), "{timings:?}");
    }
}

#[test]
fn the_slow_rule_is_immune_to_the_short_span_artefact() {
    // ⚠⚠ "p95 30 w/s, max 550" was read as the shim's encoding being unusable,
    // and it was never the encoding: every impossible rate is a sub-half-second
    // span, and a constant numerator over a tiny denominator invents a spread.
    let blink = vec![(0.0, 0.05), (0.05, 0.1)];
    assert!(
        speaking_rate(&blink).is_some_and(|r| r > 10.0),
        "the artefact is a HIGH rate"
    );
    // ⭐ Which is why the slow rule cannot be fooled by it: reaching SLOW_RATE
    // takes 1/SLOW_RATE seconds PER WORD, so no short span qualifies however
    // few words it holds.
    assert!(!is_implausibly_slow(&blink));
    assert!(!is_implausibly_slow(&[(0.0, 0.49)]));
}
