use audiocore::vad::Region;
use room::pieces::{Run, in_block_time, language_pieces, pieces, runs};

fn r(start: f64, end: f64) -> Region {
    Region { start, end }
}

fn spans(out: &[Region]) -> Vec<(f64, f64)> {
    out.iter().map(|p| (p.start, p.end)).collect()
}

#[test]
fn a_short_pause_is_inside_an_utterance() {
    let out = pieces(vec![r(0.0, 4.0), r(5.0, 9.0), r(12.0, 16.0)]);
    assert_eq!(spans(&out), vec![(0.0, 9.0), (12.0, 16.0)]);
}

#[test]
fn a_fragment_joins_the_piece_before_it() {
    let out = pieces(vec![r(0.0, 5.0), r(8.0, 8.5), r(12.0, 17.0)]);
    assert_eq!(spans(&out), vec![(0.0, 8.5), (12.0, 17.0)]);
}

#[test]
fn a_fragment_opening_the_minute_joins_the_piece_after_it() {
    let out = pieces(vec![r(0.0, 0.5), r(4.0, 9.0), r(14.0, 19.0)]);
    assert_eq!(spans(&out), vec![(0.0, 9.0), (14.0, 19.0)]);
}

#[test]
fn a_lone_fragment_is_kept_for_the_caller_to_judge() {
    let out = pieces(vec![r(10.0, 10.8)]);
    assert_eq!(spans(&out), vec![(10.0, 10.8)]);
}

#[test]
fn a_pieces_times_move_to_block_time_and_keep_its_language() {
    let result = serde_json::json!({
        "language": "nl",
        "segments": [{"start": 1.0, "end": 2.5, "text": " hallo",
                      "words": [{"start": 1.0, "end": 2.5, "text": " hallo"}]}],
    });
    let out = in_block_time(&result, 30.0);
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
fn no_pieces_is_one_run_left_to_detection() {
    assert_eq!(runs(&[], 60.0), [run(0.0, 60.0, None)]);
}

#[test]
fn language_pieces_keep_two_turns_a_short_pause_apart() {
    let turns = vec![r(0.0, 6.0), r(6.5, 12.0), r(12.2, 12.6), r(13.0, 18.0)];
    assert_eq!(spans(&pieces(turns.clone())), vec![(0.0, 18.0)]);
    assert_eq!(
        spans(&language_pieces(turns)),
        vec![(0.0, 6.0), (6.5, 12.6), (13.0, 18.0)]
    );
}
