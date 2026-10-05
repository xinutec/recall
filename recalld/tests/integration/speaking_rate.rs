//! The turn's own speaking rate, and the two ways word timings are stored.

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
    // The band's shape: four words over 32 seconds. The corpus median is
    // 2.18 w/s.
    let junk = vec![(0.0, 0.4), (11.0, 11.3), (21.0, 21.4), (32.0, 32.4)];
    assert!(is_implausibly_slow(&junk));

    // Slow, careful speech is not caught.
    let deliberate = at_rate(12, 1.0);
    assert!(!is_implausibly_slow(&deliberate));
    assert!(!is_implausibly_slow(&at_rate(20, 2.18)));
}

#[test]
fn a_turn_with_nothing_to_divide_by_is_not_accused() {
    // Words all at one instant: the timings are unusable, so no rate.
    assert_eq!(speaking_rate(&[]), None);
    assert_eq!(speaking_rate(&[(5.0, 5.0)]), None);
    assert!(!is_implausibly_slow(&[]));
    assert!(!is_implausibly_slow(&[(5.0, 5.0)]));
}

#[test]
fn the_fast_tail_gets_no_rule_because_it_is_seventeen_turns() {
    // Only 17 turns exceed 10 w/s: too few for a fast-side rule.
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
    // `diarized` writes `{s,e,w}`, turn-relative; `turns` stores the shim's
    // `{start,end,probability,text}`, clip-relative.
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
    // An absent or unreadable encoding gives no verdict.
    for timings in ["", "not json", "{}", "[]", r#"[{"probability":0.9}]"#] {
        assert!(word_spans(timings).is_empty(), "{timings:?}");
        assert!(!is_implausibly_slow(&word_spans(timings)), "{timings:?}");
    }
}

#[test]
fn the_slow_rule_is_immune_to_the_short_span_artefact() {
    // Impossibly high rates come from sub-half-second spans.
    let blink = vec![(0.0, 0.05), (0.05, 0.1)];
    assert!(
        speaking_rate(&blink).is_some_and(|r| r > 10.0),
        "the artefact is a HIGH rate"
    );
    // The slow rule needs 1/SLOW_RATE seconds per word, so no short span
    // reaches it.
    assert!(!is_implausibly_slow(&blink));
    assert!(!is_implausibly_slow(&[(0.0, 0.49)]));
}
