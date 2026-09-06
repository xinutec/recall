//! The room builder (stage D3): the calibrated rank must pick the microphone
//! hearing the room best FOR ITSELF — not the most sensitive one — blocks
//! must never be judged on partial evidence, and every verdict must carry
//! its provenance.

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
    // BURSTS, not a steady tone: the real-speech reference gate admits a
    // segment only when its speech quantile clears its own floor, and speech
    // is on-off by nature — a constant sine has no floor below itself.
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

/// Two devices with histories: `loud` normally hears speech at 0.5 (a
/// sensitive condenser), `quiet` normally at 0.02 (a gated phone). In the
/// block under test, `loud` is at its usual level while `quiet` hears 0.2 —
/// ten times its own normal. Absolute level says `loud`; calibration must
/// say `quiet`.
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

/// Mark every measured segment as carrying speech.
///
/// The reference is VAD-gated (stage D4), and these fixtures are tone bursts
/// silero will not call speech — so tests about RANKING supply the detector's
/// evidence directly. Tests about what happens WITHOUT it deliberately skip
/// this.
fn seed_speech(root: &Path) {
    let conn = store::open(root).expect("db");
    recalld::speech::ensure_schema(&conn).expect("schema");
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
fn level_evidence_without_speech_evidence_is_not_enough_to_rank() {
    // Plenty of LEVEL rows, no SPEECH rows: the reference is VAD-gated, so
    // nothing is rankable and the block must WAIT rather than be decided by
    // raw loudness — the rule the WER referee indicted twice.
    //
    // Deferral deliberately records NO verdict row: a recorded verdict is
    // terminal (a_judged_block_is_never_rejudged), so writing one here would
    // turn "wait for evidence" into "decided on the absence of it".
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
    // CALIBRATION carries `quiet` (block amplitude 0.2 → ~-17 dBFS RMS bursts),
    // not `loud` at 0.5 → ~-9. That inversion IS the feature: `quiet` is hearing
    // ten times its own normal while `loud` is at its usual level, so the block
    // belongs to the mic that suddenly hears something. This assertion read
    // `> -13.0` while the rank was parked on raw loudness (stage D3).
    assert!(speech > -21.0 && speech < -13.0, "speech {speech} dB");
    // And it registered as a segments row under the room source (the seeded
    // history minutes build their own room blocks too — assert on this one).
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
    // The seeded history blocks (10:00) are settled and may build; only the
    // 11:00 block is inside the window.
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
fn calibration_chooses_the_device_hearing_best_for_itself() {
    // The whole point of calibrating (docs/audio-plane.md): `loud` is a
    // sensitive condenser at its NORMAL level, `quiet` a gated phone at TEN
    // TIMES its own normal. Absolute level says `loud`; calibration says
    // `quiet`, because it is the one that suddenly hears something.
    //
    // This is what stage D3 parked and stage D4's detector unparks — so the
    // evidence the reference needs is the DETECTOR's, supplied here directly
    // because these fixtures are tone bursts silero would not call speech.
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
