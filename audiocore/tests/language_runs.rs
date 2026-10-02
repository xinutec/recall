use audiocore::language_runs::{Run, runs, shifted, stretches};
use audiocore::vad::Region;

fn r(start: f64, end: f64) -> Region {
    Region { start, end }
}

fn spans(out: &[Region]) -> Vec<(f64, f64)> {
    out.iter().map(|p| (p.start, p.end)).collect()
}

#[test]
fn a_runs_times_move_to_minute_time_and_keep_its_language() {
    let result = serde_json::json!({
        "language": "nl",
        "segments": [{"start": 1.0, "end": 2.5, "text": " hallo",
                      "words": [{"start": 1.0, "end": 2.5, "text": " hallo"}]}],
    });
    let out = shifted(&result, 30.0);
    assert_eq!(out[0]["start"], 31.0);
    assert_eq!(out[0]["end"], 32.5);
    assert_eq!(out[0]["words"][0]["start"], 31.0);
    assert_eq!(out[0]["language"], "nl");
}

fn run(start: f64, end: f64, language: Option<&str>) -> Run {
    Run {
        start,
        end,
        language: language.map(String::from),
    }
}

#[test]
fn one_language_is_the_whole_minute_in_that_language() {
    let p = [
        (r(3.0, 9.0), Some("nl".into())),
        (r(20.0, 30.0), Some("nl".into())),
    ];
    assert_eq!(runs(&p, 60.0), [run(0.0, 60.0, Some("nl"))]);
}

#[test]
fn a_language_change_splits_mid_pause_and_the_runs_tile_the_minute() {
    let p = [
        (r(0.0, 10.0), Some("en".into())),
        (r(14.0, 20.0), Some("nl".into())),
        (r(24.0, 30.0), Some("nl".into())),
        (r(40.0, 50.0), Some("en".into())),
    ];
    assert_eq!(
        runs(&p, 60.0),
        [
            run(0.0, 12.0, Some("en")),
            run(12.0, 35.0, Some("nl")),
            run(35.0, 60.0, Some("en"))
        ]
    );
}

#[test]
fn no_stretches_is_one_run_left_to_detection() {
    assert_eq!(runs(&[], 60.0), [run(0.0, 60.0, None)]);
}

#[test]
fn stretches_keep_two_turns_a_short_pause_apart() {
    let turns = vec![r(0.0, 6.0), r(6.5, 12.0), r(12.2, 12.6), r(13.0, 18.0)];
    assert_eq!(
        spans(&stretches(turns)),
        vec![(0.0, 6.0), (6.5, 12.6), (13.0, 18.0)]
    );
}
