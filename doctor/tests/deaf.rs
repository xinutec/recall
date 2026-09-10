//! A source that DELIVERS but hears nothing, while the mics beside it hear a
//! conversation.
//!
//! ⚠ This is the gap #1485 names: pixel5 beat, streamed and delivered on time,
//! so mic-alive, liveness and the delivery check all passed. Every signal the
//! fleet had said the phone was fine. The only thing that disagreed was what was
//! IN the audio, and nothing measured that.
//!
//! ⚠ The rule is RELATIVE, never absolute, and that is the whole design. A quiet
//! house takes every mic to zero together and means nothing is wrong; an
//! absolute floor is what once filed a phone Pippijn had deliberately switched
//! off as "a microphone in your house is missing three quarters of what's said".
//! Only a DISAGREEMENT between mics in the same minutes says anything.

use doctor::check::Verdict;
use doctor::deaf::{Heard, deaf_check};

/// The real measurement from #1485, 2026-09-08, same minutes and same room.
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

/// ⚠ THE FALSE POSITIVE THIS CHECK EXISTS TO NOT HAVE. Nobody was talking. Every
/// mic is working perfectly and every one reads zero.
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

/// ⚠ A mic that is switched OFF delivers nothing, so it is not in this list at
/// all — and must not be. Pippijn silences the pixel9 by hand, often, because he
/// types on it. Absence is the delivery check's business; this one only ever
/// speaks about sources that ARE delivering.
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

/// ⚠ The tomorrow case, and why this was written today: a microphone grant lost
/// on the MAC reads exactly like #1485's phone. audiod logs "listening", the
/// segments arrive on schedule, every existing check goes green, and the file
/// holds nothing. Here the deaf one is the always-on USB mic rather than a phone.
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

/// Too little audio to say anything. One minute is a coincidence, not a
/// measurement.
#[test]
fn too_few_shared_minutes_says_nothing_rather_than_guessing() {
    let check = deaf_check(&[
        Heard::new("usb", 60.0, 54.0),
        Heard::new("iphone11", 60.0, 53.0),
        Heard::new("oneplus6t", 60.0, 43.0),
        Heard::new("pixel5", 60.0, 0.0),
    ]);

    assert_eq!(check.verdict, Verdict::Skip);
}

/// The label is the trend identity and must not carry the source name — a check
/// whose label changes when the culprit changes has no trend at all.
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
