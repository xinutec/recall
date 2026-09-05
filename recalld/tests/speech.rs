//! Stage D4's ingest scanner: speech seconds per delivered segment.
//!
//! The real-speech case needs a real recording, and `recall` is a public repo
//! whose .gitignore refuses audio — so that test skips where the fixture is
//! absent (#1433). Everything that can run anywhere runs anywhere.

use recalld::speech::{self, UNKNOWN_SECONDS};
use recalld::store;
use std::path::Path;

/// Register a blob the scanner will find, copying `audio` into the ingest tree.
fn stored(root: &Path, source: &str, name: &str, audio: &[u8]) {
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), audio).expect("write blob");
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.to_owned(),
            filename: name.to_owned(),
            start_utc: "2026-09-05T12:00:00Z".to_owned(),
            bytes: audio.len() as u64,
            sha256: "x".to_owned(),
            received_utc: "2026-09-05T12:01:00Z".to_owned(),
            sent_utc: None,
        },
    )
    .expect("row");
}

fn silent_wav(seconds: u32) -> Vec<u8> {
    let samples = vec![0.0_f32; (16_000 * seconds) as usize];
    let path = std::env::temp_dir().join(format!("recalld-silence-{seconds}.wav"));
    audiocore::wav::write_mono16(&path, 16_000, &samples).expect("wav");
    std::fs::read(&path).expect("read")
}

#[test]
fn silence_is_measured_as_no_speech_not_as_unknown() {
    // 0.0 and UNKNOWN must never collapse: one says nobody spoke, the other
    // says nobody looked, and only one of them is safe to sweep on.
    let dir = tempfile::tempdir().expect("tempdir");
    stored(dir.path(), "usb", "usb-20260905T120000.wav", &silent_wav(3));
    assert_eq!(speech::scan_once(dir.path(), 10).expect("scan"), 1);

    let conn = store::open(dir.path()).expect("db");
    let seconds: f64 = conn
        .query_row("SELECT speech_seconds FROM segment_speech", [], |r| {
            r.get(0)
        })
        .expect("row");
    assert!(
        (seconds - 0.0).abs() < f64::EPSILON,
        "silence read as {seconds}"
    );
}

#[test]
fn an_undecodable_blob_is_recorded_as_unknown_and_never_retried() {
    let dir = tempfile::tempdir().expect("tempdir");
    stored(
        dir.path(),
        "usb",
        "usb-20260905T120000.wav",
        b"not audio at all",
    );
    assert_eq!(speech::scan_once(dir.path(), 10).expect("scan"), 1);

    let conn = store::open(dir.path()).expect("db");
    let seconds: f64 = conn
        .query_row("SELECT speech_seconds FROM segment_speech", [], |r| {
            r.get(0)
        })
        .expect("row");
    assert!((seconds - UNKNOWN_SECONDS).abs() < f64::EPSILON);
    // The row is what stops it being revisited for ever.
    assert_eq!(speech::scan_once(dir.path(), 10).expect("rescan"), 0);
}

#[test]
fn the_scan_is_idempotent_and_bounded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let quiet = silent_wav(1);
    for n in ["usb-20260905T120000.wav", "usb-20260905T120100.wav"] {
        stored(dir.path(), "usb", n, &quiet);
    }
    assert_eq!(
        speech::scan_once(dir.path(), 1).expect("scan"),
        1,
        "batch bound"
    );
    assert_eq!(
        speech::scan_once(dir.path(), 10).expect("scan"),
        1,
        "the rest"
    );
    assert_eq!(
        speech::scan_once(dir.path(), 10).expect("scan"),
        0,
        "idempotent"
    );
}

#[test]
fn a_source_with_only_silence_has_no_latest_speech() {
    let dir = tempfile::tempdir().expect("tempdir");
    stored(dir.path(), "usb", "usb-20260905T120000.wav", &silent_wav(2));
    speech::scan_once(dir.path(), 10).expect("scan");
    let conn = store::open(dir.path()).expect("db");
    assert_eq!(
        speech::latest_speech_utc(&conn, "usb").expect("query"),
        None
    );
}

#[test]
fn real_speech_is_measured_and_becomes_the_source_latest_speech() {
    let fixture = Path::new("../tests/fixtures/speech/dialogue-en.flac");
    if !fixture.exists() {
        eprintln!("skipping: speech fixture absent (public repo carries no audio)");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let audio = std::fs::read(fixture).expect("fixture");
    stored(dir.path(), "usb", "usb-20260905T120000.flac", &audio);
    assert_eq!(speech::scan_once(dir.path(), 10).expect("scan"), 1);

    let conn = store::open(dir.path()).expect("db");
    let seconds: f64 = conn
        .query_row("SELECT speech_seconds FROM segment_speech", [], |r| {
            r.get(0)
        })
        .expect("row");
    assert!(
        seconds > 15.0,
        "31 s of dialogue measured as {seconds}s speech"
    );
    assert_eq!(
        speech::latest_speech_utc(&conn, "usb")
            .expect("query")
            .as_deref(),
        Some("2026-09-05T12:00:00Z"),
    );
}
