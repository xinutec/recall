//! Forgetting a span of the household capture: the operator's deletion
//! (`recalld forget-span`), across both planes and the disk.

use crate::fixture::Fleet;
use recalld::forget::{ForgetError, Span, span};
use rusqlite::Connection;

const INSIDE: &str = "20261015T085700";
const OUTSIDE: &str = "20261015T075700";

fn the_span() -> Span {
    Span::parse("2026-10-15T08:28:00Z", "2026-10-15T09:10:00Z").expect("span")
}

fn now() -> audiocore::instant::Stamp {
    crate::stamp("2026-10-15T10:00:00+00:00")
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).expect("count")
}

/// A capture clip inside the span and one outside, a meeting upload inside it,
/// each with its file on disk, and a live line inside and outside.
fn fleet() -> Fleet {
    let fleet = Fleet::new();
    for (source, kind, start) in [
        ("usb", "coreaudio", INSIDE),
        ("usb", "coreaudio", OUTSIDE),
        ("meeting-1", "upload", INSIDE),
    ] {
        fleet.clip(
            source,
            kind,
            start,
            &[(1.0, 3.0, "the quick brown fox")],
            None,
        );
        let dir = recalld::store::source_dir(fleet.root(), source);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(format!("{source}-{start}.flac")), b"audio").expect("file");
    }
    fleet
        .meaning
        .execute_batch(
            "INSERT INTO live_lines (start_utc, end_utc, text, asr_model, created_utc) VALUES
             ('2026-10-15T08:57:17+00:00', '2026-10-15T08:57:20+00:00', 'inside', 'm', '2026-10-15T08:57:36+00:00'),
             ('2026-10-15T07:57:17+00:00', '2026-10-15T07:57:20+00:00', 'outside', 'm', '2026-10-15T07:57:36+00:00');",
        )
        .expect("live lines");
    fleet.refresh();
    fleet
}

fn file(fleet: &Fleet, source: &str, start: &str) -> std::path::PathBuf {
    recalld::store::source_dir(fleet.root(), source).join(format!("{source}-{start}.flac"))
}

fn forget(fleet: &mut Fleet, apply: bool) -> Result<recalld::forget::Forgotten, ForgetError> {
    let root = fleet.dir.path().to_owned();
    span(
        &mut fleet.meaning,
        &mut fleet.ingest,
        &root,
        &the_span(),
        &now(),
        apply,
    )
}

#[test]
fn forgetting_a_span_removes_its_capture_from_both_planes_and_the_disk() {
    let mut fleet = fleet();

    let forgotten = forget(&mut fleet, true).expect("forgotten");

    assert_eq!(forgotten.segments, 1);
    assert_eq!(forgotten.audio_rows, 1);
    assert_eq!(forgotten.live_lines, 1);
    assert!(!file(&fleet, "usb", INSIDE).exists(), "the file is gone");
    let inside = "'usb-20261015T085700.flac'";
    for (conn, sql) in [
        (&fleet.ingest, format!("SELECT count(*) FROM segments WHERE filename = {inside}")),
        (&fleet.ingest, format!("SELECT count(*) FROM clips WHERE filename = {inside}")),
        (&fleet.ingest, format!("SELECT count(*) FROM jobs WHERE filename = {inside}")),
        (
            &fleet.meaning,
            "SELECT count(*) FROM audio_segments WHERE source_id = 'usb' AND start_utc = '2026-10-15T08:57:00+00:00'".to_owned(),
        ),
        (&fleet.meaning, "SELECT count(*) FROM live_lines WHERE text = 'inside'".to_owned()),
    ] {
        assert_eq!(count(conn, &sql), 0, "{sql}");
    }
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM rendered_lines WHERE source_id = 'usb'"
        ),
        1,
        "only the line outside the span is left"
    );
}

#[test]
fn what_lies_outside_the_span_and_every_upload_are_untouched() {
    let mut fleet = fleet();

    forget(&mut fleet, true).expect("forgotten");

    assert!(file(&fleet, "usb", OUTSIDE).exists());
    assert!(
        file(&fleet, "meeting-1", INSIDE).exists(),
        "a meeting has its own delete"
    );
    assert_eq!(count(&fleet.ingest, "SELECT count(*) FROM segments"), 2);
    assert_eq!(
        count(&fleet.meaning, "SELECT count(*) FROM audio_segments"),
        2
    );
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM live_lines WHERE text = 'outside'"
        ),
        1
    );
}

#[test]
fn a_forgotten_minute_is_tombstoned() {
    let mut fleet = fleet();

    forget(&mut fleet, true).expect("forgotten");

    let start = recalld::store::lookup(&fleet.ingest, "usb-20261015T085700.flac").expect("lookup");
    assert!(start.is_none());
    let at = audiocore::instant::parse_utc("2026-10-15T08:57:00Z").expect("instant");
    assert!(recalld::upload::was_deleted(&fleet.meaning, "usb", at).expect("tombstone"));
}

#[test]
fn without_apply_it_reports_and_changes_nothing() {
    let mut fleet = fleet();

    let would = forget(&mut fleet, false).expect("report");

    assert!(!would.applied);
    assert_eq!(
        (would.segments, would.audio_rows, would.live_lines),
        (1, 1, 1)
    );
    assert!(file(&fleet, "usb", INSIDE).exists());
    assert_eq!(count(&fleet.ingest, "SELECT count(*) FROM segments"), 3);
    assert_eq!(
        count(&fleet.meaning, "SELECT count(*) FROM deleted_segments"),
        0
    );
}

#[test]
fn a_span_holding_a_voiceprints_audio_is_refused_whole() {
    let mut fleet = fleet();
    fleet
        .meaning
        .execute_batch(
            "INSERT INTO speakers (id, name) VALUES (1, 'a');
             INSERT INTO speaker_embeddings (speaker_id, vector, created_utc, source_segment_id)
             SELECT 1, '[]', '2026-10-15T09:00:00+00:00', id FROM audio_segments
             WHERE source_id = 'usb' AND start_utc = '2026-10-15T08:57:00+00:00';",
        )
        .expect("a voiceprint learnt from the span");

    let refused = forget(&mut fleet, true).expect_err("must refuse");

    assert!(
        matches!(refused, ForgetError::Voiceprint),
        "got {refused:?}"
    );
    assert!(file(&fleet, "usb", INSIDE).exists());
    assert_eq!(count(&fleet.ingest, "SELECT count(*) FROM segments"), 3);
    assert_eq!(
        count(&fleet.meaning, "SELECT count(*) FROM deleted_segments"),
        0
    );
}

#[test]
fn a_backwards_span_is_not_a_span() {
    assert!(Span::parse("2026-10-15T09:10:00Z", "2026-10-15T08:28:00Z").is_none());
    assert!(Span::parse("yesterday", "2026-10-15T08:28:00Z").is_none());
}
