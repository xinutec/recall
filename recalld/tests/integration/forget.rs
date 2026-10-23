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

// --- retiring a derived source (`recalld retire-source`) ---------------------

fn retire(
    fleet: &mut Fleet,
    source: &str,
    apply: bool,
) -> Result<recalld::forget::Forgotten, ForgetError> {
    let root = fleet.dir.path().to_owned();
    let source = transcript::SourceId::parse(source).expect("source id");
    recalld::forget::retire_source(&mut fleet.meaning, &mut fleet.ingest, &root, &source, apply)
}

/// The usb mic, and a stream built from it.
fn fleet_with_room() -> Fleet {
    let fleet = Fleet::new();
    for (source, kind) in [("usb", "coreaudio"), ("room", "derived")] {
        fleet.clip(
            source,
            kind,
            INSIDE,
            &[(1.0, 3.0, "the quick brown fox")],
            None,
        );
        let dir = recalld::store::source_dir(fleet.root(), source);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(format!("{source}-{INSIDE}.flac")), b"audio").expect("file");
    }
    fleet.refresh();
    fleet
}

#[test]
fn a_recorded_source_cannot_be_retired() {
    let mut fleet = fleet_with_room();

    assert!(matches!(
        retire(&mut fleet, "usb", true),
        Err(ForgetError::NotDerived(_))
    ));
    assert!(file(&fleet, "usb", INSIDE).exists());
    assert_eq!(
        count(
            &fleet.ingest,
            "SELECT count(*) FROM segments WHERE source = 'usb'"
        ),
        1
    );
}

#[test]
fn retiring_a_derived_source_empties_both_planes_and_the_disk_of_it_alone() {
    let mut fleet = fleet_with_room();

    let retired = retire(&mut fleet, "room", true).expect("retired");

    assert_eq!((retired.segments, retired.audio_rows), (1, 1));
    assert!(!file(&fleet, "room", INSIDE).exists());
    for (conn, sql) in [
        (
            &fleet.ingest,
            "SELECT count(*) FROM segments WHERE source = 'room'",
        ),
        (
            &fleet.ingest,
            "SELECT count(*) FROM clips WHERE source = 'room'",
        ),
        (
            &fleet.ingest,
            "SELECT count(*) FROM jobs WHERE filename LIKE 'room-%'",
        ),
        (
            &fleet.meaning,
            "SELECT count(*) FROM audio_segments WHERE source_id = 'room'",
        ),
        (
            &fleet.meaning,
            "SELECT count(*) FROM sources WHERE id = 'room'",
        ),
        (&fleet.meaning, "SELECT count(*) FROM deleted_segments"),
    ] {
        assert_eq!(count(conn, sql), 0, "{sql}");
    }
    assert!(
        file(&fleet, "usb", INSIDE).exists(),
        "the mic it was built from is the record"
    );
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM audio_segments WHERE source_id = 'usb'"
        ),
        1
    );
}

#[test]
fn retiring_without_apply_reports_and_changes_nothing() {
    let mut fleet = fleet_with_room();

    let seen = retire(&mut fleet, "room", false).expect("report");

    assert_eq!(
        (seen.applied, seen.segments, seen.audio_rows),
        (false, 1, 1)
    );
    assert!(file(&fleet, "room", INSIDE).exists());
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM sources WHERE id = 'room'"
        ),
        1
    );
}

/// A rename moves the source's id, rows, files and directory in both planes;
/// what is not its stays.
#[test]
fn renaming_a_source_moves_its_history_and_files_and_nothing_else() {
    let mut fleet = fleet();
    let root = fleet.dir.path().to_owned();
    std::fs::create_dir_all(root.join("usb")).expect("marker dir");
    fleet
        .meaning
        .execute("UPDATE sources SET owner = 'a' WHERE id = 'usb'", [])
        .expect("owner");
    // The measured tables reference segments by filename: the rename must
    // carry them, and the foreign keys must let it.
    for start in [INSIDE, OUTSIDE] {
        fleet
            .ingest
            .execute_batch(&format!(
                "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
                 VALUES ('usb-{start}.flac', 'usb', 1.0, '2026-10-15T10:00:00Z');
                 INSERT INTO segment_levels (filename, source, speech_db, floor_db, computed_utc)
                 VALUES ('usb-{start}.flac', 'usb', -20.0, -60.0, '2026-10-15T10:00:00Z');"
            ))
            .expect("child rows");
    }
    let (from, to) = (
        transcript::SourceId::parse("usb").expect("id"),
        transcript::SourceId::parse("usb-1").expect("id"),
    );
    // The recorder's status is filed under its id too.
    recalld::devices::record_beat(
        &fleet.meaning,
        &recalld::devices::Beat {
            device: "usb".into(),
            app: "recall-mic".into(),
            version: "1".into(),
            started_at: None,
            streaming: true,
            charging: None,
            mic_ok: None,
            via_lan: None,
            dropped_bytes: None,
            at: chrono::Utc::now().to_rfc3339(),
        },
        chrono::Utc::now(),
    )
    .expect("beat");
    let seen = recalld::forget::rename_source(
        &mut fleet.meaning,
        &mut fleet.ingest,
        &root,
        &from,
        &to,
        false,
    )
    .expect("report");
    assert_eq!(
        (seen.applied, seen.segments, seen.audio_rows),
        (false, 2, 2)
    );
    assert!(
        file(&fleet, "usb", INSIDE).exists(),
        "a report moves nothing"
    );

    let done = recalld::forget::rename_source(
        &mut fleet.meaning,
        &mut fleet.ingest,
        &root,
        &from,
        &to,
        true,
    )
    .expect("rename");
    assert!(done.applied);
    assert_eq!((seen.device_status, done.device_status), (1, 1));
    let beats: Vec<String> = recalld::devices::read_beats(&fleet.meaning)
        .expect("beats")
        .into_iter()
        .map(|b| b.device)
        .collect();
    assert_eq!(beats, ["usb-1"], "the heartbeat moved with the source");
    let owner: Option<String> = fleet
        .meaning
        .query_row("SELECT owner FROM sources WHERE id = 'usb-1'", [], |r| {
            r.get(0)
        })
        .expect("renamed row");
    assert_eq!(owner.as_deref(), Some("a"), "whose it is moved with it");
    assert_renamed(&fleet, &root, seen.lines);
}

/// After `usb` became `usb-1`: everything of its under the new id, the upload untouched.
fn assert_renamed(fleet: &Fleet, root: &std::path::Path, lines: usize) {
    for start in [INSIDE, OUTSIDE] {
        assert!(!file(fleet, "usb", start).exists());
        assert!(
            file(fleet, "usb-1", start).exists(),
            "{start} moved under its new name"
        );
    }
    assert!(!recalld::store::source_dir(root, "usb").exists());
    assert!(
        root.join("usb-1").is_dir() && !root.join("usb").exists(),
        "the marker dir moved"
    );
    for (plane, sql) in [
        (
            &fleet.ingest,
            "SELECT count(*) FROM segments WHERE source = 'usb'",
        ),
        (
            &fleet.ingest,
            "SELECT count(*) FROM clips WHERE source = 'usb' OR filename LIKE 'usb-2%'",
        ),
        (
            &fleet.ingest,
            "SELECT count(*) FROM jobs WHERE filename LIKE 'usb-2%'",
        ),
        (
            &fleet.meaning,
            "SELECT count(*) FROM audio_segments WHERE source_id = 'usb' OR path LIKE '%/ingest/usb/%'",
        ),
        (
            &fleet.meaning,
            "SELECT count(*) FROM rendered_lines WHERE source_id = 'usb'",
        ),
        (
            &fleet.meaning,
            "SELECT count(*) FROM sources WHERE id = 'usb'",
        ),
    ] {
        assert_eq!(count(plane, sql), 0, "{sql}");
    }
    for sql in [
        "SELECT count(*) FROM segments WHERE source = 'usb-1' AND filename LIKE 'usb-1-2%'",
        "SELECT count(*) FROM clips WHERE source = 'usb-1' AND path LIKE 'ingest/usb-1/usb-1-2%'",
        "SELECT count(*) FROM jobs WHERE filename LIKE 'usb-1-2%'",
        "SELECT count(*) FROM segment_speech WHERE source = 'usb-1' AND filename LIKE 'usb-1-2%'",
        "SELECT count(*) FROM segment_levels WHERE source = 'usb-1' AND filename LIKE 'usb-1-2%'",
    ] {
        assert_eq!(count(&fleet.ingest, sql), 2, "{sql}");
    }
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM audio_segments WHERE source_id = 'usb-1' AND path LIKE '%/ingest/usb-1/usb-1-2%'"
        ),
        2
    );
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM rendered_lines WHERE source_id = 'usb-1'"
        ),
        i64::try_from(lines).expect("lines")
    );
    assert_eq!(
        count(
            &fleet.meaning,
            "SELECT count(*) FROM sources WHERE id = 'usb-1' AND name = 'usb' AND kind = 'coreaudio'"
        ),
        1
    );
    assert_eq!(
        count(
            &fleet.ingest,
            "SELECT count(*) FROM segments WHERE source = 'meeting-1'"
        ),
        1,
        "the upload is untouched"
    );
    assert!(file(fleet, "meeting-1", INSIDE).exists());
}

#[test]
fn a_rename_refuses_a_missing_source_and_a_taken_id() {
    let mut fleet = fleet();
    let root = fleet.dir.path().to_owned();
    let id = |s: &str| transcript::SourceId::parse(s).expect("id");
    let missing = recalld::forget::rename_source(
        &mut fleet.meaning,
        &mut fleet.ingest,
        &root,
        &id("nobody"),
        &id("usb-1"),
        true,
    );
    assert!(matches!(missing, Err(ForgetError::NoSuchSource(_))));
    let taken = recalld::forget::rename_source(
        &mut fleet.meaning,
        &mut fleet.ingest,
        &root,
        &id("usb"),
        &id("meeting-1"),
        true,
    );
    assert!(matches!(taken, Err(ForgetError::SourceExists(_))));
    assert!(
        file(&fleet, "usb", INSIDE).exists(),
        "a refusal moves nothing"
    );
}
