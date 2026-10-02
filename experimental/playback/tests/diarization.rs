use chrono::{DateTime, Duration, TimeZone, Utc};
use playback::diarization::{Clip, Turn, score};

fn at(s: f64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
        + Duration::milliseconds((s * 1000.0) as i64)
}

fn turn(start: f64, end: f64, who: &str) -> Turn {
    Turn {
        start: at(start),
        end: at(end),
        speaker: who.into(),
    }
}

#[test]
fn labels_match_readers_inside_a_clip_and_the_rest_is_confusion() {
    let reference = [turn(0.0, 10.0, "a"), turn(11.0, 20.0, "b")];
    let clips = [Clip {
        source: "mic".into(),
        turns: vec![
            // Covers a, then runs 3 s into b: one mixed turn, 3 s confused.
            turn(0.0, 14.0, "S0"),
            turn(14.0, 18.0, "S1"),
        ],
    }];
    let s = &score(&reference, &clips)["mic"];
    // a: 10 s; b inside the clip's span (11-18): 7 s, of which 18-20 lies outside.
    assert!((s.reference_s - 17.0).abs() < 1e-6, "{s:?}");
    assert!(s.uncovered_s.abs() < 1e-6, "{s:?}");
    assert!((s.confused_s - 3.0).abs() < 1e-6, "{s:?}");
    assert_eq!((s.turns, s.mixed_turns), (2, 1));
    assert_eq!((s.clips, s.clips_miscounted), (1, 0));
}

#[test]
fn speech_no_turn_covers_is_uncovered_and_one_label_for_two_readers_miscounts() {
    let reference = [turn(0.0, 5.0, "a"), turn(6.0, 12.0, "b")];
    let clips = [Clip {
        source: "mic".into(),
        turns: vec![turn(0.0, 3.0, "S0"), turn(7.0, 12.0, "S0")],
    }];
    let s = &score(&reference, &clips)["mic"];
    assert!((s.uncovered_s - 3.0).abs() < 1e-6, "{s:?}");
    assert!((s.confused_s - 3.0).abs() < 1e-6, "{s:?}");
    assert_eq!(s.clips_miscounted, 1);
}
