//! What the fleet can say about the instant feed, out of both planes at once.

use audiocore::instant::python_isoformat_utc;
use recalld::live_tier::live_health;
use recalld::store;
use rusqlite::Connection;
use std::path::Path;

const WINDOW_SINCE: &str = "2026-09-21T11:40:00+00:00";
const WINDOW_UNTIL: &str = "2026-09-21T12:00:00+00:00";

/// The meaning plane, built by the real migration ladder.
fn meaning(root: &Path) -> Connection {
    let conn = Connection::open(root.join("recall.sqlite")).expect("db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

fn source(conn: &Connection, id: &str, kind: &str) {
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES (?1, ?1, ?2)",
        [id, kind],
    )
    .expect("source");
}

/// A delivered clip, and — when `speech` is given — its measured speech, which
/// lives in the OTHER database.
fn clip(root: &Path, source_id: &str, name: &str, minute: u32, speech: Option<f64>) {
    let start = format!("2026-09-21T11:{minute:02}:00+00:00");
    let end = format!("2026-09-21T11:{minute:02}:30+00:00");
    Connection::open(root.join("recall.sqlite"))
        .expect("db")
        .execute(
            "INSERT INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, 16000, 1)",
            [
                source_id,
                &format!("/data/ingest/{source_id}/{name}"),
                &start,
                &end,
            ],
        )
        .expect("clip");
    // The blob's own row first: `segment_speech.filename` references it.
    let conn = store::open(root).expect("ingest");
    recalld::ingest_schema::ensure(&conn).expect("schema");
    store::insert(
        &conn,
        &store::Row {
            source: source_id.to_owned(),
            filename: name.to_owned(),
            start_utc: start.clone(),
            bytes: 1,
            sha256: "x".to_owned(),
            received_utc: start.clone(),
            sent_utc: None,
        },
    )
    .expect("blob");
    if let Some(seconds) = speech {
        conn.execute(
            "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
             VALUES (?1, ?2, ?3, '2026-09-21T12:00:00Z')",
            rusqlite::params![name, source_id, seconds],
        )
        .expect("speech");
    }
}

/// A turn ending 30 s into `minute`, stored `lag_s` after it ended.
fn turn(conn: &Connection, model: &str, minute: u32, lag_s: i64) {
    let end = at(&format!("2026-09-21T11:{minute:02}:30+00:00"));
    conn.execute(
        "INSERT INTO transcript_segments (asr_model, start_utc, end_utc, created_utc, text)
         VALUES (?1, ?2, ?3, ?4, 'x')",
        [
            model,
            &format!("2026-09-21T11:{minute:02}:00+00:00"),
            &python_isoformat_utc(end),
            &python_isoformat_utc(end + chrono::Duration::seconds(lag_s)),
        ],
    )
    .expect("turn");
}

fn at(stamp: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(stamp)
        .expect("an instant")
        .into()
}

fn health(root: &Path) -> recalld::live_tier::LiveHealth {
    live_health(root, at(WINDOW_SINCE), at(WINDOW_SINCE), at(WINDOW_UNTIL)).expect("health")
}

#[test]
fn the_lag_is_the_median_and_only_over_the_live_tier() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning(dir.path());
    // The median, not the mean: one turn that waited behind a restart moves a
    // mean by minutes.
    for (minute, lag) in [(41, 3), (42, 4), (43, 5), (44, 6), (45, 200)] {
        turn(&conn, "live", minute, lag);
    }
    // An archive pass writes the same table and must not be counted: it is
    // hours behind by design, and its lag would swamp the feed's.
    turn(&conn, "large-v3-turbo", 46, 9_000);
    drop(conn);

    let out = health(dir.path());
    assert_eq!(out.lag_samples, 5, "the archive pass is not the live tier");
    assert_eq!(out.lag_median_s, Some(5.0));
}

#[test]
fn a_window_reports_what_was_delivered_and_how_much_of_it_was_measured() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning(dir.path());
    source(&conn, "usb", "coreaudio");
    drop(conn);
    clip(
        dir.path(),
        "usb",
        "usb-20260921T114100.flac",
        41,
        Some(20.0),
    );
    clip(dir.path(), "usb", "usb-20260921T114200.flac", 42, None);

    let out = health(dir.path());
    assert!((out.delivered_s - 60.0).abs() < 0.001, "two 30 s clips");
    assert!((out.scanned_s - 30.0).abs() < 0.001, "one of them measured");
    assert!((out.speech_s - 20.0).abs() < 0.001);
}

#[test]
fn a_clip_that_would_not_decode_is_unmeasured_rather_than_silent() {
    // ⚠ UNKNOWN_SECONDS is negative so it cannot pass for a duration. Summed, it
    // would subtract speech and push a busy window under the floor that decides
    // whether anybody spoke, blaming a working tier for silence.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning(dir.path());
    source(&conn, "usb", "coreaudio");
    drop(conn);
    clip(
        dir.path(),
        "usb",
        "usb-20260921T114100.flac",
        41,
        Some(25.0),
    );
    clip(
        dir.path(),
        "usb",
        "usb-20260921T114200.flac",
        42,
        Some(recalld::speech::UNKNOWN_SECONDS),
    );

    let out = health(dir.path());
    assert!(
        (out.speech_s - 25.0).abs() < 0.001,
        "the unknown did not vote"
    );
    assert!(
        (out.scanned_s - 30.0).abs() < 0.001,
        "an undecodable clip is delivered but NOT scanned: {}",
        out.scanned_s
    );
}

#[test]
fn only_sources_with_a_recorder_count_as_delivered_audio() {
    // An imported meeting and the derived room stream are sources with no
    // microphone. Counting either would say the room was busy when a file was
    // uploaded, or double-count the microphone the room stream carried.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning(dir.path());
    source(&conn, "usb", "coreaudio");
    source(&conn, "meeting-20260921-1100", "upload");
    source(&conn, "room", "derived");
    drop(conn);
    clip(
        dir.path(),
        "usb",
        "usb-20260921T114100.flac",
        41,
        Some(10.0),
    );
    clip(
        dir.path(),
        "meeting-20260921-1100",
        "meeting-20260921T114200.flac",
        42,
        Some(30.0),
    );
    clip(
        dir.path(),
        "room",
        "room-20260921T114300.flac",
        43,
        Some(30.0),
    );

    let out = health(dir.path());
    assert!(
        (out.delivered_s - 30.0).abs() < 0.001,
        "the microphone only"
    );
    assert!((out.speech_s - 10.0).abs() < 0.001);
}

#[test]
fn an_empty_archive_answers_with_no_opinion_rather_than_a_zero() {
    // "Nothing to measure" is not "measured and fine": a lag of 0.0 would read as
    // a perfect feed.
    let dir = tempfile::tempdir().expect("tempdir");
    drop(meaning(dir.path()));
    let out = health(dir.path());
    assert_eq!(out.lag_median_s, None);
    assert_eq!(out.lag_samples, 0);
    assert_eq!(out.newest_turn_utc, None);
    assert!((out.delivered_s - 0.0).abs() < f64::EPSILON);
}

#[test]
fn heard_is_per_microphone_and_counts_only_measured_clips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = meaning(dir.path());
    source(&conn, "usb", "coreaudio");
    source(&conn, "pixel5", "tcp_pcm");
    source(&conn, "room", "derived");
    drop(conn);
    clip(
        dir.path(),
        "usb",
        "usb-20260921T114100.flac",
        41,
        Some(20.0),
    );
    clip(dir.path(), "usb", "usb-20260921T114200.flac", 42, None);
    clip(
        dir.path(),
        "pixel5",
        "pixel5-20260921T114100.flac",
        41,
        Some(0.0),
    );
    clip(
        dir.path(),
        "room",
        "room-20260921T114300.flac",
        43,
        Some(30.0),
    );

    let heard =
        recalld::live_tier::heard(dir.path(), at(WINDOW_SINCE), at(WINDOW_UNTIL)).expect("heard");
    let got: Vec<(&str, f64, f64)> = heard
        .iter()
        .map(|h| (h.source.as_str(), h.delivered_s, h.speech_s))
        .collect();
    assert_eq!(got, [("pixel5", 30.0, 0.0), ("usb", 30.0, 20.0)]);
}
