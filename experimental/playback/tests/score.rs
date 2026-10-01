use chrono::{DateTime, Duration, TimeZone, Utc};
use playback::plan::{Part, Plan, Turn};
use playback::score::{Line, Played, room_lines, score, total};

fn at(s: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap() + Duration::seconds(s)
}

fn part(name: &str, text: &str) -> Part {
    Part {
        name: name.into(),
        device: "Speaker".into(),
        seconds: 30.0,
        turns: vec![Turn {
            offset: 1.0,
            duration: 5.0,
            speaker: "a".into(),
            lang: "en".into(),
            text: text.into(),
            audio: String::new(),
        }],
    }
}

fn line(source: &str, start: i64, end: i64, text: &str) -> Line {
    Line {
        source: source.into(),
        start: at(start),
        end: at(end),
        text: text.into(),
    }
}

#[test]
fn lines_go_to_the_part_holding_their_midpoint_and_strays_are_invented() {
    let plan = Plan {
        seed: 1,
        parts: vec![part("one", "red green blue"), part("two", "one two three")],
    };
    let played = vec![
        Played {
            part: "one".into(),
            device: "Speaker".into(),
            start: at(100),
        },
        Played {
            part: "two".into(),
            device: "Speaker".into(),
            start: at(200),
        },
    ];
    let lines = vec![
        // Starts before part one but its midpoint (102) is inside.
        line("mic", 96, 108, "red green"),
        line("mic", 110, 112, "blue"),
        line("mic", 201, 206, "one two tree"),
        // Between the parts: nobody spoke.
        line("mic", 150, 160, "thank you for watching"),
        // Outside the window: ignored.
        line("mic", 20, 30, "far away"),
        line("other", 101, 106, "red green blue"),
    ];
    let report = score(&plan, &played, &lines, at(60), at(270));
    let one = report
        .parts
        .iter()
        .find(|p| p.source == "mic" && p.part == "one")
        .unwrap();
    assert_eq!(one.errors.rate(), Some(0.0));
    let two = report
        .parts
        .iter()
        .find(|p| p.source == "mic" && p.part == "two")
        .unwrap();
    assert_eq!(two.errors.substitutions, 1);
    let inv = |s: &str| {
        report
            .invented
            .iter()
            .find(|i| i.source == s)
            .unwrap()
            .words
    };
    assert_eq!(inv("mic"), 4);
    assert_eq!(inv("other"), 0);
    // A source that heard nothing of part two is scored as deleting all of it.
    let other_two = report
        .parts
        .iter()
        .find(|p| p.source == "other" && p.part == "two")
        .unwrap();
    assert_eq!(other_two.errors.deletions, 3);
    assert_eq!(total(&report, "mic").reference, 6);
    assert!((report.silent_seconds - (210.0 - 60.0)).abs() < 1e-9);
}

#[test]
fn room_output_becomes_one_source_per_arm_on_the_wall_clock() {
    let jsonl = concat!(
        r#"{"block":"2026-10-01T12:01:00Z","winner":"usb","arm":"whole","result":{"segments":[{"start":1.5,"end":4.0,"text":" red green"}]}}"#,
        "\n",
        r#"{"block":"2026-10-01T12:01:00Z","winner":"usb","arm":"pieces","result":{"segments":[]}}"#,
        "\n",
    );
    let lines = room_lines(jsonl).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].source, "room-whole");
    assert_eq!(lines[0].start, at(61) + Duration::milliseconds(500));
    assert_eq!(lines[0].end, at(64));
    assert!(room_lines("{\"arm\":\"whole\"}").is_err());
}
