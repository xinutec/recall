//! Conversation and moment folding: the two groupings that give an always-on
//! capture stream a shape a person can browse.

use chrono::{DateTime, TimeDelta, Utc};
use recalld::conversations::{
    DEFAULT_GAP_SECONDS, Turn, best_colocated_guess, cluster_moments, fold, segment_conversations,
};

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_780_000_000 + seconds, 0).expect("in range")
}

/// A turn with everything the folds ignore left empty.
fn turn(id: i64, start: i64, end: i64, source: &str) -> Turn {
    Turn {
        id,
        start: at(start),
        end: at(end),
        speaker_label: None,
        source_id: Some(source.to_owned()),
        asr_confidence: None,
        speaker_guess: None,
        speaker_score: None,
    }
}

fn scored(mut t: Turn, confidence: f64) -> Turn {
    t.asr_confidence = Some(confidence);
    t
}

fn guessed(mut t: Turn, name: &str, score: f64) -> Turn {
    t.speaker_guess = Some(name.to_owned());
    t.speaker_score = Some(score);
    t
}

#[test]
fn a_silence_longer_than_the_gap_starts_a_new_conversation() {
    let turns = vec![
        turn(1, 0, 10, "usb"),
        turn(2, 20, 30, "usb"),
        // 400s of silence: past the 300s default.
        turn(3, 430, 440, "usb"),
    ];

    let groups = segment_conversations(&turns, DEFAULT_GAP_SECONDS);

    assert_eq!(groups, vec![vec![0, 1], vec![2]]);
}

#[test]
fn the_silence_is_measured_from_the_furthest_end_not_the_previous_turn() {
    // A long turn from one mic covers a short one from another. Measuring the gap
    // from the short turn's end would invent a silence and split the
    // conversation.
    let turns = vec![
        turn(1, 0, 500, "usb"),   // runs long
        turn(2, 10, 20, "pixel"), // ends early, inside the first
        turn(3, 600, 610, "usb"), // 100s after the real end: no break
    ];

    let groups = segment_conversations(&turns, DEFAULT_GAP_SECONDS);

    assert_eq!(groups, vec![vec![0, 1, 2]], "one conversation, not two");
}

#[test]
fn a_gap_exactly_on_the_threshold_does_not_break() {
    // The rule is strictly greater than, so a caller calibrating the knob gets
    // the boundary they asked for.
    let turns = vec![turn(1, 0, 10, "usb"), turn(2, 310, 320, "usb")];

    assert_eq!(segment_conversations(&turns, 300.0), vec![vec![0, 1]]);
    assert_eq!(
        segment_conversations(&turns, 299.9),
        vec![vec![0], vec![1]],
        "just under the gap, it breaks"
    );
}

#[test]
fn one_utterance_heard_by_three_mics_folds_into_one_moment() {
    let turns = vec![
        scored(turn(1, 0, 10, "usb"), 0.9),
        scored(turn(2, 1, 11, "pixel"), 0.4),
        scored(turn(3, 2, 9, "oneplus"), 0.3),
        // A separate utterance, no overlap.
        scored(turn(4, 60, 70, "usb"), 0.8),
    ];
    let group: Vec<usize> = (0..turns.len()).collect();

    let moments = cluster_moments(&turns, &group);

    assert_eq!(moments.len(), 2, "two utterances, not four turns");
    assert_eq!(moments[0].primary, vec![0], "the clearest mic is the spine");
    assert_eq!(moments[0].alternates, vec![1, 2]);
    assert_eq!(moments[1].primary, vec![3]);
}

#[test]
fn the_spine_is_the_source_with_the_highest_summed_confidence() {
    // Two turns from a middling mic beat one turn from a slightly better one,
    // because the sum is what is compared — the finer speaker split wins.
    let turns = vec![
        scored(turn(1, 0, 10, "usb"), 0.55),
        scored(turn(2, 0, 4, "pixel"), 0.3),
        scored(turn(3, 4, 10, "pixel"), 0.3),
    ];
    let group: Vec<usize> = (0..turns.len()).collect();

    let moments = cluster_moments(&turns, &group);

    assert_eq!(moments[0].primary, vec![1, 2], "0.6 beats 0.55");
    assert_eq!(moments[0].alternates, vec![0]);
}

#[test]
fn a_tied_spine_goes_to_the_first_source_seen_not_the_last() {
    // ⚠ On a full tie the first microphone is the spine. Rust's `max_by_key`
    // returns the last maximum, and swapping the spine silently swaps which
    // transcription the UI shows as primary and which it hides behind "compare".
    let turns = vec![
        scored(turn(1, 0, 10, "usb"), 0.5),
        scored(turn(2, 1, 11, "pixel"), 0.5),
    ];
    let group = vec![0, 1];

    let moments = cluster_moments(&turns, &group);

    assert_eq!(
        moments[0].primary,
        vec![0],
        "usb appeared first, so usb is the spine"
    );
}

#[test]
fn a_missing_guess_is_filled_from_the_most_confident_overlapping_mic() {
    let turns = vec![
        turn(1, 0, 10, "usb"),                           // spine, no guess of its own
        guessed(turn(2, 1, 9, "pixel"), "Alex", 0.3),    // overlaps
        guessed(turn(3, 2, 8, "oneplus"), "Sam", 0.7),   // overlaps, stronger
        guessed(turn(4, 50, 60, "pixel"), "Robin", 0.9), // no overlap: ignored
    ];

    let chosen = best_colocated_guess(&turns, &[0], &[1, 2, 3]);

    assert_eq!(
        chosen[&1],
        (Some("Sam".to_owned()), Some(0.7)),
        "the strongest co-located guess, and not the non-overlapping one"
    );
}

#[test]
fn an_existing_guess_is_strengthened_by_agreement_but_never_flipped() {
    // The asymmetry is deliberate. Phone clocks are arrival-stamped and lag by a
    // variable few seconds, so a time overlap is not reliable evidence of the same
    // speaker: a mic that agrees may raise the confidence, one that disagrees must
    // not rename the person.
    let turns = vec![
        guessed(turn(1, 0, 10, "usb"), "Alex", 0.4),
        guessed(turn(2, 1, 9, "pixel"), "Alex", 0.8), // agrees, stronger
        guessed(turn(3, 2, 8, "oneplus"), "Sam", 0.95), // disagrees, strongest
    ];

    let chosen = best_colocated_guess(&turns, &[0], &[1, 2]);

    assert_eq!(
        chosen[&1],
        (Some("Alex".to_owned()), Some(0.8)),
        "Alex at the corroborated strength — never Sam, however confident"
    );
}

#[test]
fn a_weaker_agreeing_mic_does_not_lower_the_confidence() {
    let turns = vec![
        guessed(turn(1, 0, 10, "usb"), "Alex", 0.8),
        guessed(turn(2, 1, 9, "pixel"), "Alex", 0.2),
    ];

    let chosen = best_colocated_guess(&turns, &[0], &[1]);

    assert_eq!(chosen[&1], (Some("Alex".to_owned()), Some(0.8)));
}

#[test]
fn turns_that_only_touch_are_not_overlapping() {
    // Adjacency is not overlap: one turn ending exactly where the next begins is
    // two moments, not one folded card.
    let turns = vec![turn(1, 0, 10, "usb"), turn(2, 10, 20, "pixel")];
    let group = vec![0, 1];

    let moments = cluster_moments(&turns, &group);

    assert_eq!(moments.len(), 2);
}

#[test]
fn an_empty_stream_folds_to_nothing_rather_than_panicking() {
    let turns: Vec<Turn> = Vec::new();

    assert!(segment_conversations(&turns, DEFAULT_GAP_SECONDS).is_empty());
    assert!(cluster_moments(&turns, &[]).is_empty());
}

#[test]
fn a_turn_with_no_source_still_folds() {
    // Corrections carry no audio segment, so their source is NULL. They must not
    // vanish from the timeline.
    let mut orphan = turn(1, 0, 10, "usb");
    orphan.source_id = None;
    let turns = vec![orphan, turn(2, 1, 9, "usb")];

    let moments = cluster_moments(&turns, &[0, 1]);

    assert_eq!(moments.len(), 1);
    assert_eq!(moments[0].primary.len() + moments[0].alternates.len(), 2);
}

#[test]
fn a_long_conversation_keeps_its_turns_in_order() {
    let turns: Vec<Turn> = (0..50)
        .map(|i| turn(i, i * 20, i * 20 + 10, "usb"))
        .collect();

    let groups = segment_conversations(&turns, DEFAULT_GAP_SECONDS);

    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0], (0..50).map(|i| i as usize).collect::<Vec<_>>());
}

#[test]
fn a_gap_measured_in_fractions_of_a_second_is_respected() {
    // The threshold is a float the caller sets; truncating to whole seconds
    // would put this pair on the wrong side of it.
    let turns = vec![
        Turn {
            end: at(0) + TimeDelta::milliseconds(500),
            ..turn(1, 0, 0, "usb")
        },
        Turn {
            start: at(2),
            ..turn(2, 2, 3, "usb")
        },
    ];

    // The real gap is 1.5s.
    assert_eq!(segment_conversations(&turns, 1.6), vec![vec![0, 1]]);
    assert_eq!(segment_conversations(&turns, 1.4), vec![vec![0], vec![1]]);
}

// --- the conversation SUMMARY, which is what a browsing list shows -----------
//
// These pin `fold`, which builds `ConversationOut`: `turnCount`, `speakers` and
// `preview`, the fields the list view is made of. The tests above work on turn
// indices only.

/// A stored row, with everything the summary ignores left empty.
fn segment(id: i64, start: i64, end: i64, text: &str) -> recalld::reads::Segment {
    recalld::reads::Segment {
        id,
        start_utc: at(start).to_rfc3339(),
        end_utc: at(end).to_rfc3339(),
        text: text.to_owned(),
        language: None,
        asr_confidence: None,
        loudness: None,
        asr_model: Some("whisper".to_owned()),
        speaker_label: None,
        speaker_guess: None,
        speaker_score: None,
        speaker_cluster: None,
        provenance: None,
        hidden_reason: None,
        source_id: Some("usb".to_owned()),
        words_checked: false,
    }
}

fn named(mut s: recalld::reads::Segment, who: &str) -> recalld::reads::Segment {
    s.speaker_label = Some(who.to_owned());
    s
}

fn confident(mut s: recalld::reads::Segment, confidence: f64) -> recalld::reads::Segment {
    s.asr_confidence = Some(confidence);
    s
}

#[test]
fn a_conversation_reports_its_span_its_turn_count_and_its_confirmed_speakers() {
    let segments = vec![
        named(segment(1, 0, 2, "first"), "Carol"),
        named(segment(2, 3, 5, "second"), "Alice"),
        named(segment(3, 7, 9, "third"), "Carol"),
        // No confirmed name: counted as a turn, absent from the speaker list.
        segment(4, 10, 12, "fourth"),
    ];

    let out = fold(&segments, 60.0, 200);

    assert_eq!(out.items.len(), 1, "one conversation");
    let conv = &out.items[0];
    assert_eq!(conv.turn_count, 4);
    assert_eq!(conv.start, reads_iso(0));
    assert_eq!(conv.end, reads_iso(12));
    // Distinct, first-seen order, and nothing for the unnamed turn.
    assert_eq!(conv.speakers, vec!["Carol".to_owned(), "Alice".to_owned()]);
}

/// A card is headed by its first line above `PREVIEW_MIN_CONFIDENCE`, so a
/// low-confidence guess is not what a person reads first.
#[test]
fn the_preview_skips_a_low_confidence_opening_line() {
    let segments = vec![
        confident(segment(1, 0, 2, "mumbled nonsense"), 0.1),
        confident(segment(2, 3, 5, "the line worth showing"), 0.9),
    ];

    let out = fold(&segments, 60.0, 200);

    assert_eq!(out.items[0].preview, "the line worth showing");
}

/// When nothing clears the bar the card still gets a heading: an empty preview
/// reads as an empty conversation.
#[test]
fn a_conversation_with_nothing_confident_still_previews_its_first_line() {
    let segments = vec![
        confident(segment(1, 0, 2, "mumbled nonsense"), 0.1),
        confident(segment(2, 3, 5, "more of the same"), 0.2),
    ];

    let out = fold(&segments, 60.0, 200);

    assert_eq!(out.items[0].preview, "mumbled nonsense");
}

/// An unscored turn is not treated as confident: a NULL confidence means nobody
/// measured.
#[test]
fn an_unscored_line_does_not_clear_the_preview_bar() {
    let segments = vec![
        segment(1, 0, 2, "unscored"),
        confident(segment(2, 3, 5, "scored and good"), 0.9),
    ];

    let out = fold(&segments, 60.0, 200);

    assert_eq!(out.items[0].preview, "scored and good");
}

/// The gap is tunable, and the summary splits with it, as a caller passing
/// `?gap=` sees.
#[test]
fn the_gap_is_tunable_and_the_summary_splits_with_it() {
    let segments = vec![segment(1, 0, 2, "before"), segment(2, 40, 42, "after")];

    assert_eq!(fold(&segments, 60.0, 200).items.len(), 1, "38s < 60s gap");
    assert_eq!(fold(&segments, 10.0, 200).items.len(), 2, "38s > 10s gap");
}

/// The instants are the stored strings, not re-formatted ones: the pass-through
/// `reads::iso` promises, checked on a conversation card.
fn reads_iso(seconds: i64) -> String {
    at(seconds).to_rfc3339()
}
