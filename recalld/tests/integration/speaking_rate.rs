//! The turn's own speaking rate, and the two ways word timings are stored. The
//! wordless and repetition-loop rules are tested in `audiocore::text`.

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
    // The measured shape of the band: four words over 32 seconds. The corpus
    // median is 2.18 w/s, conversational speed, so a rule that caught ordinary
    // slow talking would catch the archive.
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
    // `None`, neither infinitely fast nor slow: words that all carry the same
    // instant mean the timings are unusable, and a verdict on that would grade
    // the encoding, not the speech.
    assert_eq!(speaking_rate(&[]), None);
    assert_eq!(speaking_rate(&[(5.0, 5.0)]), None);
    assert!(!is_implausibly_slow(&[]));
    assert!(!is_implausibly_slow(&[(5.0, 5.0)]));
}

#[test]
fn the_fast_tail_gets_no_rule_because_it_is_seventeen_turns() {
    // Only 17 turns in the corpus exceed 10 w/s, too few for a rule whose false
    // positives would not outnumber its finds.
    assert!(!is_implausibly_slow(&at_rate(8, 30.0)));
    assert!(speaking_rate(&at_rate(8, 30.0)).is_some_and(|r| r > 10.0));
}

#[test]
fn the_cut_is_read_from_the_rule_rather_than_copied_beside_it() {
    // A threshold pasted into a test drifts from the one that ships.
    let just_under = at_rate(6, SLOW_RATE * 0.9);
    let just_over = at_rate(6, SLOW_RATE * 1.1);
    assert!(is_implausibly_slow(&just_under));
    assert!(!is_implausibly_slow(&just_over));
}

#[test]
fn both_stored_timing_encodings_are_read() {
    // `diarized` writes recalld's own `{s,e,w}`, re-based to the turn; `turns`
    // stores the shim's reply verbatim as `{start,end,probability,text}`,
    // absolute within the clip.
    let ours = r#"[{"s":0.0,"e":0.4,"w":"one"},{"s":11.0,"e":11.3,"w":"two"}]"#;
    let shims = r#"[{"start":32.0,"end":32.4,"probability":0.9,"text":"one"},
                    {"start":43.0,"end":43.3,"probability":0.8,"text":"two"}]"#;
    assert_eq!(word_spans(ours), vec![(0.0, 0.4), (11.0, 11.3)]);
    assert_eq!(word_spans(shims), vec![(32.0, 32.4), (43.0, 43.3)]);
    // The same junk, spelled both ways, gets the same verdict.
    assert!(is_implausibly_slow(&word_spans(ours)));
    assert!(is_implausibly_slow(&word_spans(shims)));
}

#[test]
fn a_turn_with_no_usable_timings_accuses_nobody() {
    // An absent or unreadable encoding reads as no opinion: grading the encoding
    // would zero confidence on every turn whose shape it does not know.
    for timings in ["", "not json", "{}", "[]", r#"[{"probability":0.9}]"#] {
        assert!(word_spans(timings).is_empty(), "{timings:?}");
        assert!(!is_implausibly_slow(&word_spans(timings)), "{timings:?}");
    }
}

#[test]
fn the_slow_rule_is_immune_to_the_short_span_artefact() {
    // Impossibly high rates come from sub-half-second spans: a small word count
    // over a tiny denominator.
    let blink = vec![(0.0, 0.05), (0.05, 0.1)];
    assert!(
        speaking_rate(&blink).is_some_and(|r| r > 10.0),
        "the artefact is a HIGH rate"
    );
    // Which is why the slow rule cannot be fooled by it: reaching SLOW_RATE
    // takes 1/SLOW_RATE seconds PER WORD, so no short span qualifies however
    // few words it holds.
    assert!(!is_implausibly_slow(&blink));
    assert!(!is_implausibly_slow(&[(0.0, 0.49)]));
}
