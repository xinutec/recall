//! A source that delivers but hears nothing while the mics beside it hear a
//! conversation. Beats, liveness and delivery all pass for such a source; only
//! the audio's content disagrees.
//!
//! The rule is relative, never absolute: a quiet house takes every mic to zero
//! together and means nothing is wrong. Only disagreement between mics in the
//! same minutes says anything.

use doctor::check::Verdict;
use doctor::deaf::{Heard, deaf_check};

/// A real measurement: four mics, same minutes, same room.
fn measured() -> Vec<Heard> {
    vec![
        Heard::new("usb", 300.0, 54.2 * 5.0),
        Heard::new("iphone11", 300.0, 53.8 * 5.0),
        Heard::new("oneplus6t", 300.0, 43.3 * 5.0),
        Heard::new("pixel5", 300.0, 0.0),
    ]
}

#[test]
fn the_one_mic_hearing_nothing_while_three_hear_a_conversation_is_named() {
    let check = deaf_check(&measured());

    assert_eq!(check.verdict, Verdict::Warn);
    assert!(
        check.observed.contains("pixel5"),
        "the deaf source must be named: {}",
        check.observed
    );
    for peer in ["usb", "iphone11", "oneplus6t"] {
        assert!(
            !check.observed.contains(peer),
            "{peer} heard speech and must not be accused: {}",
            check.observed
        );
    }
}

/// The false positive this check must not have: nobody talking, every mic working and reading zero.
#[test]
fn a_quiet_house_is_not_a_broken_microphone() {
    let quiet = vec![
        Heard::new("usb", 300.0, 0.0),
        Heard::new("iphone11", 300.0, 0.0),
        Heard::new("oneplus6t", 300.0, 0.0),
        Heard::new("pixel5", 300.0, 0.0),
    ];

    let check = deaf_check(&quiet);

    assert_eq!(
        check.verdict,
        Verdict::Skip,
        "silence everywhere is an absence of evidence, not a fault"
    );
}

/// A mic switched off delivers nothing and is absent from the list, often
/// deliberately. Absence is the delivery check's business; this one speaks only
/// about sources that are delivering.
#[test]
fn a_source_that_delivered_nothing_is_not_accused_of_deafness() {
    let check = deaf_check(&[
        Heard::new("usb", 300.0, 54.2 * 5.0),
        Heard::new("iphone11", 300.0, 53.8 * 5.0),
        Heard::new("oneplus6t", 300.0, 43.3 * 5.0),
    ]);

    assert_eq!(check.verdict, Verdict::Pass);
    assert!(!check.observed.contains("pixel9"));
}

/// One peer is not a quorum: if only one other mic heard anything, the one that
/// heard it is as likely to be the odd one out as the one that did not.
#[test]
fn one_peer_hearing_speech_is_not_enough_to_accuse_another() {
    let check = deaf_check(&[
        Heard::new("usb", 300.0, 54.2 * 5.0),
        Heard::new("pixel5", 300.0, 0.0),
    ]);

    assert_eq!(check.verdict, Verdict::Skip);
}

/// A handful of seconds is not a conversation. Two mics catching a door closing
/// must not convict a third of deafness.
#[test]
fn a_trace_of_sound_in_the_peers_is_not_a_conversation() {
    let check = deaf_check(&[
        Heard::new("usb", 300.0, 1.0),
        Heard::new("iphone11", 300.0, 1.5),
        Heard::new("oneplus6t", 300.0, 0.5),
        Heard::new("pixel5", 300.0, 0.0),
    ]);

    assert_eq!(check.verdict, Verdict::Skip);
}

/// A microphone grant lost on the Mac looks the same: segments arrive on
/// schedule, every other check is green, and the audio holds nothing.
#[test]
fn the_macs_own_mic_going_silent_is_caught_the_same_way() {
    let check = deaf_check(&[
        Heard::new("usb", 360.0, 0.0),
        Heard::new("iphone11", 360.0, 50.0 * 6.0),
        Heard::new("oneplus6t", 360.0, 44.0 * 6.0),
        Heard::new("pixel5", 360.0, 47.0 * 6.0),
    ]);

    assert_eq!(check.verdict, Verdict::Warn);
    assert!(check.observed.contains("usb"), "{}", check.observed);
}

/// The label is the trend identity, so it must not carry the source name.
#[test]
fn the_label_is_stable_whichever_mic_is_deaf() {
    let a = deaf_check(&measured());
    let b = deaf_check(&[
        Heard::new("usb", 300.0, 0.0),
        Heard::new("iphone11", 300.0, 53.8 * 5.0),
        Heard::new("oneplus6t", 300.0, 43.3 * 5.0),
        Heard::new("pixel5", 300.0, 47.0 * 5.0),
    ]);

    assert_eq!(a.label, b.label);
    assert_eq!(a.verdict, Verdict::Warn);
    assert_eq!(b.verdict, Verdict::Warn);
}

/// A real single-segment measurement: four mics heard 21-25 s of speech and
/// pixel5 heard nothing. One complete segment must be enough to judge, since
/// peer agreement, not duration, is what makes a zero meaningful.
#[test]
fn the_real_2026_09_10_measurement_names_pixel5() {
    // speech seconds within a single 60 s segment, straight off the archive.
    let heard = vec![
        // Phones close segments at 59.993 s, not 60; a round-number fixture
        // would hide a floor set at exactly 60.
        Heard::new("iphone11", 59.993, 25.3),
        Heard::new("oneplus6t", 59.993, 22.8),
        Heard::new("pixel9", 59.993, 21.8),
        Heard::new("usb", 60.0, 20.9),
        Heard::new("pixel5", 59.993, 0.0),
    ];

    let check = deaf_check(&heard);

    assert_eq!(check.verdict, Verdict::Warn, "{}", check.observed);
    assert!(check.observed.contains("pixel5"), "{}", check.observed);
    for peer in ["usb", "iphone11", "oneplus6t", "pixel9"] {
        assert!(!check.observed.contains(peer), "{}", check.observed);
    }
}

/// The floor must still refuse something: half a segment, from a source that
/// started mid-minute or was cut off by a pause, is too little to convict on.
#[test]
fn half_a_segment_is_still_too_little_to_judge() {
    let check = deaf_check(&[
        Heard::new("iphone11", 30.0, 12.0),
        Heard::new("oneplus6t", 30.0, 11.0),
        Heard::new("usb", 30.0, 10.0),
        Heard::new("pixel5", 30.0, 0.0),
    ]);

    assert_eq!(check.verdict, Verdict::Skip, "{}", check.observed);
}

#[test]
fn a_fleet_that_could_not_be_asked_skips_saying_why() {
    let check = doctor::deaf::deaf_check_from(&Err("cannot reach the fleet (timeout)".to_owned()));
    assert_eq!(check.verdict, Verdict::Skip);
    assert!(
        check.observed.contains("cannot reach the fleet"),
        "{}",
        check.observed
    );
    assert_eq!(check.label, deaf_check(&measured()).label);
}
