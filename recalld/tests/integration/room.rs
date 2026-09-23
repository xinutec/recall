//! The room builder: blocks are never judged on partial evidence, every
//! verdict carries its provenance, and the calibrated rank (parked) picks the
//! microphone hearing best for itself rather than the most sensitive one.

use chrono::{DateTime, Duration, Utc};
use recalld::levels::scan_once;
use recalld::room::{BuildSummary, ROOM_SOURCE, RoomConfig, build_once, verdict_of};
use recalld::store;
use std::f32::consts::PI;
use std::path::Path;

fn config() -> RoomConfig {
    RoomConfig {
        settle: Duration::minutes(15),
        batch: 30,
        reference_quantile: 0.05,
        reference_window: 100,
        // Tests seed a short history; production keeps its higher floor.
        min_reference_rows: 3,
    }
}

fn wav(path: &Path, amplitude: f32, seconds: f32) {
    let rate = 16_000u32;
    // Bursts, not a steady tone: the reference admits a segment only when its
    // speech quantile clears its own floor, and a constant sine has no floor
    // below itself.
    let samples: Vec<f32> = (0..(seconds * rate as f32) as usize)
        .map(|i| {
            let on = (i / rate as usize).is_multiple_of(2);
            let gain = if on { amplitude } else { amplitude * 0.001 };
            gain * (2.0 * PI * 330.0 * i as f32 / rate as f32).sin()
        })
        .collect();
    audiocore::wav::write_mono16(path, rate, &samples).expect("wav");
}

fn stored(root: &Path, source: &str, stamp: &str, amplitude: f32) {
    let name = format!("{source}-{stamp}.wav");
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    wav(&dir.join(&name), amplitude, 60.0);
    let start = DateTime::parse_from_str(&format!("{stamp}+0000"), "%Y%m%dT%H%M%S%z")
        .expect("stamp")
        .with_timezone(&Utc);
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.to_owned(),
            filename: name,
            start_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

/// Two devices with histories: `loud` normally at 0.5 (a sensitive
/// condenser), `quiet` normally at 0.02 (a phone). In the 11:00 block `loud`
/// is at its usual level while `quiet` hears ten times its own normal, so
/// absolute level says `loud` and calibration says `quiet`.
fn seed_two_devices(root: &Path) -> DateTime<Utc> {
    for i in 0..4 {
        stored(root, "loud", &format!("20260905T1000{i:02}"), 0.5);
        stored(root, "quiet", &format!("20260905T1000{i:02}"), 0.02);
    }
    stored(root, "loud", "20260905T110000", 0.5);
    stored(root, "quiet", "20260905T110000", 0.2);
    DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc)
}

/// Mark every measured segment as carrying speech. The reference only counts
/// VAD speech and silero does not call tone bursts speech, so ranking tests
/// supply that evidence directly; tests about its absence skip this.
fn seed_speech(root: &Path) {
    let conn = store::open(root).expect("db");
    recalld::ingest_schema::ensure(&conn).expect("schema");
    conn.execute(
        "INSERT OR IGNORE INTO segment_speech (filename, source, speech_seconds, computed_utc)
         SELECT filename, source, 30.0, '2026-09-05T10:00:00Z' FROM segment_levels",
        [],
    )
    .expect("speech rows");
}

fn now_after(block: DateTime<Utc>) -> DateTime<Utc> {
    block + Duration::minutes(30)
}

#[test]
#[ignore = "re-parked 2026-09-06 with calibrated selection — see room.rs"]
fn level_evidence_without_speech_evidence_is_not_enough_to_rank() {
    // Level rows but no speech rows: nothing is rankable, so the block waits
    // rather than being decided by raw loudness. Deferral records no verdict,
    // because a recorded verdict is terminal (a_judged_block_is_never_rejudged).
    let dir = tempfile::tempdir().expect("tempdir");
    let block = seed_two_devices(dir.path());
    scan_once(dir.path(), 100).expect("levels");
    let summary = build_once(dir.path(), &config(), now_after(block)).expect("build");
    assert_eq!(summary.built, 0, "{summary:?}");
    assert!(summary.deferred > 0, "{summary:?}");
    let conn = store::open(dir.path()).expect("db");
    assert_eq!(
        verdict_of(&conn, "2026-09-05T11:00:00Z").expect("q"),
        None,
        "a deferred block must stay unjudged so a later pass can build it"
    );
}

#[test]
fn the_room_blob_carries_the_winners_audio() {
    let dir = tempfile::tempdir().expect("tempdir");
    let block = seed_two_devices(dir.path());
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());
    build_once(dir.path(), &config(), now_after(block)).expect("build");
    let blob = dir
        .path()
        .join("ingest")
        .join(ROOM_SOURCE)
        .join("room-20260905T110000.flac");
    let pcm = audiocore::decode::decode_s16(&blob, 16_000).expect("decodable");
    let envelope = audiocore::envelope::rms_buckets_at(&pcm, 16_000, 0.1);
    let speech = audiocore::envelope::level_quantile_db(&envelope, 0.9);
    // Raw level chooses, so `loud` carries the block: amplitude 0.5 reads about
    // -9 dBFS, where `quiet`'s 0.2 would read about -17.
    assert!(speech > -13.0 && speech < -3.0, "speech {speech} dB");
    // It is also a segments row under the room source. The seeded history
    // builds room blocks too, so assert on this one.
    let conn = store::open(dir.path()).expect("db");
    let rows = store::list(&conn, Some(ROOM_SOURCE), None, 10).expect("list");
    assert!(
        rows.iter()
            .any(|r| r.filename == "room-20260905T110000.flac"),
        "{rows:?}"
    );
}

#[test]
fn no_verdict_on_partial_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let block = seed_two_devices(dir.path());
    // Levels NOT scanned: every block must defer, none may be judged.
    let summary = build_once(dir.path(), &config(), now_after(block)).expect("build");
    assert_eq!(summary.built, 0, "{summary:?}");
    assert!(summary.deferred > 0);
    let conn = store::open(dir.path()).expect("db");
    assert_eq!(verdict_of(&conn, "2026-09-05T11:00:00Z").expect("q"), None);
    // Once measured, the same pass shape builds it.
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());
    let after = build_once(dir.path(), &config(), now_after(block)).expect("build");
    assert!(after.built >= 1);
}

#[test]
#[ignore = "re-parked 2026-09-06: the corpus cannot test calibrated selection \
            (48% divergence, 1.7% ground-truth overlap) — see room.rs"]
fn no_reference_means_deferred_not_degraded() {
    let dir = tempfile::tempdir().expect("tempdir");
    // One segment only: measured, but far under min_reference_rows.
    stored(dir.path(), "solo", "20260905T110000", 0.3);
    scan_once(dir.path(), 100).expect("levels");
    let block = DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc);
    let summary = build_once(dir.path(), &config(), now_after(block)).expect("build");
    assert_eq!(
        (summary.built, summary.silent),
        (0, 0),
        "an unrankable block must not fall back to raw loudness: {summary:?}"
    );
    assert!(summary.deferred > 0);
}

#[test]
fn an_unsettled_block_is_not_judged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let block = seed_two_devices(dir.path());
    scan_once(dir.path(), 100).expect("levels");
    // "Now" is one minute after the block: inside the settling window.
    let summary = build_once(dir.path(), &config(), block + Duration::minutes(1)).expect("build");
    let conn = store::open(dir.path()).expect("db");
    assert_eq!(verdict_of(&conn, "2026-09-05T11:00:00Z").expect("q"), None);
    // The 10:00 history blocks are settled and may build, so the summary
    // counts are not asserted.
    let _ = summary;
}

#[test]
fn a_judged_block_is_never_rejudged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let block = seed_two_devices(dir.path());
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());
    let first = build_once(dir.path(), &config(), now_after(block)).expect("build");
    let second = build_once(dir.path(), &config(), now_after(block)).expect("build");
    assert!(first.built >= 1);
    assert_eq!(second, BuildSummary::default(), "everything already judged");
}

#[test]
#[ignore = "re-parked 2026-09-06: calibration is recorded, not obeyed — see room.rs"]
fn calibration_chooses_the_device_hearing_best_for_itself() {
    // The point of calibrating (docs/architecture.md): `quiet` wins because it
    // is the one that suddenly hears something, though `loud` is louder.
    let dir = tempfile::tempdir().expect("tempdir");
    let block = seed_two_devices(dir.path());
    scan_once(dir.path(), 100).expect("levels");

    seed_speech(dir.path());

    let summary = build_once(dir.path(), &config(), now_after(block)).expect("build");
    assert!(summary.built >= 1, "{summary:?}");
    let conn = store::open(dir.path()).expect("db");
    let (verdict, winner): (String, String) = conn
        .query_row(
            "SELECT verdict, winner FROM room_blocks WHERE start_utc = ?1",
            ["2026-09-05T11:00:00Z"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("block row");
    assert_eq!(verdict, "built:calibrated", "the rule must be recorded");
    assert_eq!(
        winner, "quiet",
        "calibration must pick the device hearing best FOR ITSELF, not the loudest"
    );
}

// ---- gated sources are removed before the rank (filter parked) ----

use recalld::room::GATED_MAX;

/// A gating source: bursts separated by true digital silence, which a
/// speakerphone emits and no analogue front end can.
fn gated_wav(path: &Path, amplitude: f32, seconds: f32) {
    let rate = 16_000u32;
    let samples: Vec<f32> = (0..(seconds * rate as f32) as usize)
        .map(|i| {
            // 0.4 s of tone, then 0.6 s of absolute zero, each second.
            let phase = i % rate as usize;
            if phase < (rate as usize * 2) / 5 {
                amplitude * (2.0 * PI * 330.0 * i as f32 / rate as f32).sin()
            } else {
                0.0
            }
        })
        .collect();
    audiocore::wav::write_mono16(path, rate, &samples).expect("wav");
}

fn stored_gated(root: &Path, source: &str, stamp: &str, amplitude: f32) {
    let name = format!("{source}-{stamp}.wav");
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    gated_wav(&dir.join(&name), amplitude, 60.0);
    let start = DateTime::parse_from_str(&format!("{stamp}+0000"), "%Y%m%dT%H%M%S%z")
        .expect("stamp")
        .with_timezone(&Utc);
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.to_owned(),
            filename: name,
            start_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

#[test]
#[ignore = "parked 2026-09-12: every phone gates during speech, so the filter separates phones from the condenser rather than broken from working — see room.rs"]
fn the_loudest_source_loses_when_it_is_gating() {
    // Gating makes a source read louder, so a rank that merely weighted it
    // would still pick it. It has to be removed from the candidates.
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path();
    stored_gated(root, "geb", "20260911T100000", 0.9);
    stored(root, "usb", "20260911T100000", 0.3);
    scan_once(root, 100).expect("levels");

    let block = DateTime::parse_from_str("20260911T100000+0000", "%Y%m%dT%H%M%S%z")
        .expect("stamp")
        .with_timezone(&Utc);
    let conn = store::open(root).expect("db");
    let summary = build_once(root, &config(), now_after(block)).expect("build");
    assert_eq!(summary.built, 1, "a clean source was available");
    let winner: String = conn
        .query_row("SELECT winner FROM room_blocks LIMIT 1", [], |r| r.get(0))
        .expect("verdict");
    assert_eq!(
        winner, "usb",
        "the quieter UNGATED source must win over the louder gated one"
    );
}

#[test]
#[ignore = "parked 2026-09-12: every phone gates during speech, so the filter separates phones from the condenser rather than broken from working — see room.rs"]
fn a_minute_where_every_source_gates_builds_nothing() {
    // Not `silent`, and not the least-bad source: the room was not quiet, and a
    // block built from the least-gated source would look like an honest one.
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path();
    stored_gated(root, "geb", "20260911T100000", 0.9);
    stored_gated(root, "pixel5", "20260911T100000", 0.5);
    scan_once(root, 100).expect("levels");

    let block = DateTime::parse_from_str("20260911T100000+0000", "%Y%m%dT%H%M%S%z")
        .expect("stamp")
        .with_timezone(&Utc);
    let summary = build_once(root, &config(), now_after(block)).expect("build");
    assert_eq!(summary.built, 0, "nothing honest to build");
    assert_eq!(summary.gated, 1, "counted as gated, not as silence");
    let conn = store::open(root).expect("db");
    let verdict: String = conn
        .query_row("SELECT verdict FROM room_blocks LIMIT 1", [], |r| r.get(0))
        .expect("verdict");
    assert_eq!(verdict, "all-gated");
}

#[test]
fn a_segment_measured_before_the_detector_defers_the_block() {
    // NULL `gated` means never measured, not measured clean; ranking on it
    // would be a partial-evidence verdict. `scan_once` backfills it.
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path();
    stored(root, "usb", "20260911T100000", 0.3);
    scan_once(root, 100).expect("levels");
    let conn = store::open(root).expect("db");
    conn.execute("UPDATE segment_levels SET gated = NULL", [])
        .expect("blank it");

    let block = DateTime::parse_from_str("20260911T100000+0000", "%Y%m%dT%H%M%S%z")
        .expect("stamp")
        .with_timezone(&Utc);
    let summary = build_once(root, &config(), now_after(block)).expect("build");
    assert_eq!(
        summary.built, 0,
        "an unmeasured gate reading is not evidence"
    );
    assert_eq!(summary.deferred, 1);
}

#[test]
fn the_backfill_re_measures_a_row_whose_gate_reading_is_missing() {
    // A NULL must be fillable, or the builder defers those blocks forever.
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path();
    stored(root, "usb", "20260911T100000", 0.3);
    scan_once(root, 100).expect("levels");
    let conn = store::open(root).expect("db");
    conn.execute("UPDATE segment_levels SET gated = NULL", [])
        .expect("blank it");

    assert_eq!(
        scan_once(root, 100).expect("backfill"),
        1,
        "it must revisit"
    );
    let gated: Option<f64> = conn
        .query_row("SELECT gated FROM segment_levels LIMIT 1", [], |r| r.get(0))
        .expect("row");
    assert!(gated.is_some(), "the backfill must fill it, not skip it");
    assert!(
        gated.expect("some") <= f64::from(GATED_MAX),
        "a bursting sine is not a gate"
    );
}

/// A clip covering only `seconds` of its minute, as a pause, dropout or
/// late-starting recorder leaves.
fn stored_short(root: &Path, source: &str, stamp: &str, amplitude: f32, seconds: f32) {
    let name = format!("{source}-{stamp}.wav");
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    wav(&dir.join(&name), amplitude, seconds);
    let start = DateTime::parse_from_str(&format!("{stamp}+0000"), "%Y%m%dT%H%M%S%z")
        .expect("stamp")
        .with_timezone(&Utc);
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.to_owned(),
            filename: name,
            start_utc: start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

fn block_row(root: &Path, block: DateTime<Utc>) -> (String, Option<f64>) {
    let conn = store::open(root).expect("db");
    conn.query_row(
        "SELECT verdict, coverage FROM room_blocks WHERE start_utc = ?1",
        [block.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .expect("verdict row")
}

#[test]
fn a_block_the_winner_barely_recorded_is_refused_not_padded() {
    // One clip starting at :50 covers ten seconds of the minute; the other
    // fifty would be zero-filled and transcribed. The all-zero guard cannot
    // see it, because the block is not all zero.
    let dir = tempfile::tempdir().expect("tmp");
    // A history, so the source is rankable at all.
    for i in 0..4 {
        stored(dir.path(), "usb", &format!("20260905T1000{i:02}"), 0.5);
    }
    // The block under test: ten seconds of audio in a sixty second minute.
    stored_short(dir.path(), "usb", "20260905T110050", 0.5, 10.0);
    let block = DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc);
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());

    build_once(dir.path(), &config(), now_after(block)).expect("build");

    // Assert on the block under test, not the pass counts: the history clips
    // start a second apart, so their tails leave the following minute sparsely
    // covered and refused too.
    let (verdict, coverage) = block_row(dir.path(), block);
    assert_eq!(verdict, "sparse");
    let coverage = coverage.expect("the coverage must be RECORDED, not merely acted on");
    assert!(
        coverage < 0.25,
        "ten seconds of sixty is about 0.17; got {coverage:.3}"
    );
}

#[test]
fn a_fully_covered_block_still_builds_and_records_its_coverage() {
    // Refusing sparse blocks must not refuse ordinary ones, and coverage is
    // recorded either way so the floor can be raised from the distribution.
    let dir = tempfile::tempdir().expect("tmp");
    for i in 0..4 {
        stored(dir.path(), "usb", &format!("20260905T1000{i:02}"), 0.5);
    }
    stored(dir.path(), "usb", "20260905T110000", 0.5);
    let block = DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc);
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());

    build_once(dir.path(), &config(), now_after(block)).expect("build");

    let (verdict, coverage) = block_row(dir.path(), block);
    assert!(verdict.starts_with("built"), "got {verdict}");
    assert!(
        coverage.expect("recorded") > 0.9,
        "a whole minute of audio covers the whole minute"
    );
}

#[test]
fn coverage_is_backfilled_for_blocks_judged_before_it_was_measured() {
    // Blocks judged before coverage was measured read NULL, meaning unknown,
    // not fully covered.
    let dir = tempfile::tempdir().expect("tmp");
    for i in 0..4 {
        stored(dir.path(), "usb", &format!("20260905T1000{i:02}"), 0.5);
    }
    stored_short(dir.path(), "usb", "20260905T110050", 0.5, 10.0);
    let block = DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc);
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());
    build_once(dir.path(), &config(), now_after(block)).expect("build");

    // Blank the column, as on a database from before it existed.
    let conn = store::open(dir.path()).expect("db");
    conn.execute("UPDATE room_blocks SET coverage = NULL", [])
        .expect("clear");

    let measured = recalld::room::backfill_coverage(dir.path(), 100).expect("backfill");
    assert!(measured > 0, "there were blocks to measure");
    let (verdict, coverage) = block_row(dir.path(), block);
    assert_eq!(verdict, "sparse", "the verdict is not rewritten");
    assert!(
        coverage.expect("measured") < 0.25,
        "ten seconds of sixty is about 0.17"
    );

    assert_eq!(
        recalld::room::backfill_coverage(dir.path(), 100).expect("again"),
        0,
        "a measured block is not measured again"
    );
}

// --- the per-source gate signature -------------------------------------------
//
// The signature is the speech-to-floor gap in `processed.rs`. `quiet_run_s` is
// stored as evidence but no rule reads it.

/// The gap signature is recorded on every contributor and decides nothing, so
/// the rule can be judged from provenance before it ever acts.
#[test]
fn every_contributor_records_the_gap_signature_without_it_changing_the_verdict() {
    let dir = tempfile::tempdir().expect("tmp");
    for i in 0..12 {
        stored(dir.path(), "usb", &format!("20260905T1000{i:02}"), 0.5);
    }
    stored(dir.path(), "usb", "20260905T110000", 0.5);
    let block = DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc);
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());

    build_once(dir.path(), &config(), now_after(block)).expect("build");

    let conn = store::open(dir.path()).expect("db");
    let (verdict, contributors): (String, String) = conn
        .query_row(
            "SELECT verdict, contributors FROM room_blocks WHERE start_utc = ?1",
            [block.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");

    // serde writes the key even for `None`, so the value must be a number.
    assert!(
        !contributors.contains("\"gap_db\":null"),
        "twelve measured segments must produce a signature, got {contributors}"
    );
    assert!(
        contributors.contains("\"gap_db\":"),
        "the signature is persisted as provenance, got {contributors}"
    );
    assert!(
        verdict.starts_with("built"),
        "recording a signature must not drop the source, got {verdict}"
    );
}

/// A source with too little history records no signature, so a new microphone
/// is not judged on its first day.
#[test]
fn too_few_segments_record_no_gap_signature() {
    let dir = tempfile::tempdir().expect("tmp");
    stored(dir.path(), "usb", "20260905T110000", 0.5);
    let block = DateTime::parse_from_rfc3339("2026-09-05T11:00:00Z")
        .expect("t")
        .with_timezone(&Utc);
    scan_once(dir.path(), 100).expect("levels");
    seed_speech(dir.path());

    build_once(dir.path(), &config(), now_after(block)).expect("build");

    let conn = store::open(dir.path()).expect("db");
    let contributors: String = conn
        .query_row(
            "SELECT contributors FROM room_blocks WHERE start_utc = ?1",
            [block.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)],
            |r| r.get(0),
        )
        .expect("row");
    assert!(
        contributors.contains("\"gap_db\":null"),
        "one clip is below MIN_SEGMENTS, got {contributors}"
    );
}
