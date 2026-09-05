//! Calibration evidence from delivered segments (stage D2): a loud segment
//! and a quiet one from the same "device" must order correctly, the scanner
//! must be idempotent, and the per-device reference must come from the rows.

use recalld::levels::{scan_once, speech_reference_db};
use recalld::store;
use std::f32::consts::PI;
use std::path::Path;

/// A mono 16 kHz WAV of a sine at `amplitude` — through audiocore's writer,
/// decoded back by the real ffmpeg path in `measure`.
fn wav_segment(path: &Path, amplitude: f32, seconds: f32) {
    let rate = 16_000u32;
    let samples: Vec<f32> = (0..(seconds * rate as f32) as usize)
        .map(|i| amplitude * (2.0 * PI * 440.0 * i as f32 / rate as f32).sin())
        .collect();
    audiocore::wav::write_mono16(path, rate, &samples).expect("wav");
}

fn stored_segment(root: &Path, source: &str, name: &str, amplitude: f32) {
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    wav_segment(&dir.join(name), amplitude, 2.0);
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.to_owned(),
            filename: name.to_owned(),
            start_utc: name[name.len() - 19..name.len() - 4].to_owned(),
            bytes: 1,
            sha256: "x".to_owned(),
            received_utc: "2026-09-05T00:00:00Z".to_owned(),
            sent_utc: None,
        },
    )
    .expect("row");
}

#[test]
fn levels_order_by_how_loud_the_device_heard() {
    let dir = tempfile::tempdir().expect("tempdir");
    stored_segment(dir.path(), "usb", "usb-20260905T120000.wav", 0.5);
    stored_segment(dir.path(), "usb", "usb-20260905T120100.wav", 0.005);
    assert_eq!(scan_once(dir.path(), 100).expect("scan"), 2);
    let conn = store::open(dir.path()).expect("db");
    let loud: f64 = conn
        .query_row(
            "SELECT speech_db FROM segment_levels WHERE filename LIKE '%120000%'",
            [],
            |r| r.get(0),
        )
        .expect("loud");
    let quiet: f64 = conn
        .query_row(
            "SELECT speech_db FROM segment_levels WHERE filename LIKE '%120100%'",
            [],
            |r| r.get(0),
        )
        .expect("quiet");
    // 0.5 vs 0.005 amplitude = 40 dB apart; allow decode slop either side.
    assert!(loud > quiet + 30.0, "loud {loud} vs quiet {quiet}");
}

#[test]
fn the_scanner_measures_each_blob_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    stored_segment(dir.path(), "usb", "usb-20260905T120000.wav", 0.2);
    assert_eq!(scan_once(dir.path(), 100).expect("scan"), 1);
    assert_eq!(scan_once(dir.path(), 100).expect("rescan"), 0);
}

#[test]
fn an_undecodable_blob_is_recorded_not_retried_forever() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("ingest").join("usb");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("usb-20260905T120000.wav"), b"not audio").expect("junk");
    let conn = store::open(dir.path()).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: "usb".into(),
            filename: "usb-20260905T120000.wav".into(),
            start_utc: "2026-09-05T12:00:00Z".into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
    assert_eq!(scan_once(dir.path(), 100).expect("scan"), 1);
    assert_eq!(scan_once(dir.path(), 100).expect("rescan"), 0);
}

#[test]
fn the_device_reference_is_a_query_over_its_own_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (i, amp) in [0.5f32, 0.4, 0.3, 0.05].iter().enumerate() {
        stored_segment(
            dir.path(),
            "usb",
            &format!("usb-20260905T12000{i}.wav"),
            *amp,
        );
    }
    scan_once(dir.path(), 100).expect("scan");
    let conn = store::open(dir.path()).expect("db");
    let faintest = speech_reference_db(&conn, "usb", 0.0, 100)
        .expect("query")
        .expect("some");
    let loudest = speech_reference_db(&conn, "usb", 1.0, 100)
        .expect("query")
        .expect("some");
    assert!(faintest < loudest - 15.0, "{faintest} vs {loudest}");
    // A device with no measured rows has no reference — never a default.
    assert_eq!(
        speech_reference_db(&conn, "pixel5", 0.05, 100).expect("query"),
        None
    );
}
