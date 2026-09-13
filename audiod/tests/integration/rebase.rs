use audiocore::names::parse_segment_start;
use audiod::rebase::{connection_offset, rebase_segment_names};
use chrono::{TimeZone, Utc};
use std::collections::HashSet;
use std::path::Path;

#[test]
fn offset_is_capture_minus_arrival_clamped_to_zero() {
    assert_eq!(connection_offset(None, 100.0), None);
    assert_eq!(connection_offset(Some(96.0), 100.0), Some(-4.0)); // buffered audio
    assert_eq!(connection_offset(Some(102.0), 100.0), Some(0.0)); // never forward
    assert_eq!(connection_offset(Some(1000.0), 100.0), None); // untrusted clock
}

#[test]
fn segment_start_parses_from_anywhere_in_the_name() {
    let ts = parse_segment_start("pixel9-20260904T190601.opus").expect("parses");
    assert_eq!(ts, Utc.with_ymd_and_hms(2026, 9, 4, 19, 6, 1).unwrap());
    assert_eq!(parse_segment_start(".alive"), None);
    assert_eq!(parse_segment_start("pixel9-2026.opus"), None);
}

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), b"x").unwrap();
}

#[test]
fn rebase_shifts_closed_segments_and_spares_the_open_one() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "p-20260904T190000.opus");
    touch(dir.path(), "p-20260904T190100.opus"); // newest = ffmpeg's open segment
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 0, 0).unwrap();
    let mut done = HashSet::new();
    let renamed = rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, false);
    assert_eq!(
        renamed,
        vec![(
            "p-20260904T190000.opus".into(),
            "p-20260904T185956.opus".into()
        )]
    );
    assert!(dir.path().join("p-20260904T185956.opus").exists());
    assert!(dir.path().join("p-20260904T190100.opus").exists()); // untouched
}

#[test]
fn a_rebased_name_is_never_shifted_twice() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "p-20260904T190000.opus");
    touch(dir.path(), "p-20260904T190100.opus");
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 0, 0).unwrap();
    let mut done = HashSet::new();
    rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, false);
    // Second sweep: the renamed file is in `done`, so nothing moves again.
    let again = rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, false);
    assert!(again.is_empty());
    assert!(dir.path().join("p-20260904T185956.opus").exists());
}

#[test]
fn an_earlier_connections_segment_is_not_ours_to_move() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "p-20260904T180000.opus"); // before `since`
    touch(dir.path(), "p-20260904T190100.opus");
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 30, 0).unwrap();
    let mut done = HashSet::new();
    let renamed = rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, true);
    assert_eq!(renamed.len(), 1); // only the in-window file moved
    assert!(dir.path().join("p-20260904T180000.opus").exists());
}

#[test]
fn a_taken_slot_keeps_the_arrival_name_forever() {
    let dir = tempfile::tempdir().unwrap();
    // The corrected slot is occupied by an EARLIER connection's segment —
    // one this sweep will not move (it is stamped before `since`).
    touch(dir.path(), "p-20260904T185952.opus");
    touch(dir.path(), "p-20260904T185956.opus");
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 59, 55).unwrap();
    let mut done = HashSet::new();
    let renamed = rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, true);
    // Losing audio to a rename would invert priority #1: ours keeps its
    // arrival name, the occupier is untouched.
    assert!(renamed.is_empty());
    assert!(dir.path().join("p-20260904T185952.opus").exists());
    assert!(dir.path().join("p-20260904T185956.opus").exists());
}

#[test]
fn a_slot_vacated_within_the_sweep_is_reusable() {
    let dir = tempfile::tempdir().unwrap();
    // Two of OUR segments 4 s apart, shifting by -4: the first vacates the
    // slot the second lands in. Both move — the whole connection shifts as
    // one consistent timeline (the Python server behaves identically).
    touch(dir.path(), "p-20260904T185956.opus");
    touch(dir.path(), "p-20260904T190000.opus");
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 0, 0).unwrap();
    let mut done = HashSet::new();
    let renamed = rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, true);
    assert_eq!(renamed.len(), 2);
    assert!(dir.path().join("p-20260904T185952.opus").exists());
    assert!(dir.path().join("p-20260904T185956.opus").exists());
}

#[test]
fn the_final_sweep_may_move_the_newest_segment() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "p-20260904T190000.opus");
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 0, 0).unwrap();
    let mut done = HashSet::new();
    let renamed = rebase_segment_names(dir.path(), "p", -4.0, &mut done, since, true);
    assert_eq!(renamed.len(), 1);
}

#[test]
fn a_sub_half_second_offset_moves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    touch(dir.path(), "p-20260904T190000.opus");
    let since = Utc.with_ymd_and_hms(2026, 9, 4, 18, 0, 0).unwrap();
    let mut done = HashSet::new();
    assert!(rebase_segment_names(dir.path(), "p", -0.4, &mut done, since, true).is_empty());
}
