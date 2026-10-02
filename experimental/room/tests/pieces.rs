use audiocore::vad::Region;
use room::pieces::pieces;

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
