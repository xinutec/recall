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

/// ⚠ **THE LIVE CASE, and the check could not see it.** Measured 2026-09-10
/// 20:35Z with Pippijn alone in the house reading a known script: every mic
/// delivered ONE 60-second segment, four heard 21-25 s of him, and pixel5 heard
/// nothing and produced no turns. That is exactly what this check exists to
/// name — and it skipped, because `MIN_DELIVERED_S` was 180 s and one segment is
/// sixty.
///
/// The 180 was a guess made without data. What it guards against is a source
/// that delivered only during a quiet patch, so its zero means nothing — and
/// that risk is ABSENT when four peers each heard twenty seconds over the very
/// same minute. The protection that matters is peer agreement, not duration, so
/// the floor is now one complete segment: enough to have heard something.
///
/// ⚠ Not lowered to make a test pass. Lowered because a real case showed the
/// bound excluding evidence that was already decisive.
#[test]
fn the_real_2026_09_10_measurement_names_pixel5() {
    // speech seconds within a single 60 s segment, straight off the archive.
    let heard = vec![
        // ⚠ 59.993, the MEASURED duration — not the nominal 60.0. The phones
        // close a few milliseconds short and only the Mac's capture hits 60.000,
        // so a fixture using the round number tests a segment that never exists
        // and hides a floor set at exactly 60.
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

/// ⚠ The floor still has to REFUSE something, or lowering it was just deleting a
/// guard. Half a segment — a source that started mid-minute or was cut off by a
/// pause — is still too little to convict on.
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
