//! E1 lifecycle, lean: derive → newest-first lease → done; a lapsed lease
//! re-offers.

use chrono::{DateTime, Duration, Utc};
use recalld::queue::{done, lease};
use recalld::store;

fn room_row(root: &std::path::Path, stamp: &str) {
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: "room".into(),
            filename: format!("room-{stamp}.flac"),
            start_utc: stamp.into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
}

#[test]
fn newest_first_lease_done_and_lapse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let now: DateTime<Utc> = "2026-09-05T12:00:00Z".parse().expect("t");
    room_row(dir.path(), "20260905T100000");
    room_row(dir.path(), "20260905T110000");
    // Newest first.
    let first = lease(dir.path(), now).expect("lease").expect("job");
    assert_eq!(first.filename, "room-20260905T110000.flac");
    // The leased job is not re-offered while its lease holds…
    let second = lease(dir.path(), now).expect("lease").expect("job");
    assert_eq!(second.filename, "room-20260905T100000.flac");
    assert!(lease(dir.path(), now).expect("lease").is_none());
    // …but a lapsed lease re-offers, and done retires for good.
    let later = now + Duration::minutes(20);
    let again = lease(dir.path(), later).expect("lease").expect("job");
    assert_eq!(again.filename, "room-20260905T110000.flac");
    assert!(done(dir.path(), again.id, "{}", later).expect("done"));
    assert!(!done(dir.path(), again.id, "{}", later).expect("idempotent"));
    let last = lease(dir.path(), later + Duration::minutes(20))
        .expect("lease")
        .expect("job");
    assert_eq!(last.filename, "room-20260905T100000.flac");
}

#[test]
fn a_segment_measured_as_silent_gets_no_transcription_job() {
    // Transcribing silence does not return nothing — it returns INVENTIONS.
    // Measured on the live queue 2026-09-06: a silent minute came back as
    // "Thank you." twice, another as 156 segments with a 150-character run of
    // tildes at 0.19 confidence (#1410). 42% of the queue was silence.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = store::open(dir.path()).expect("db");
    recalld::queue::ensure_schema(&conn).expect("jobs schema");
    recalld::speech::ensure_schema(&conn).expect("speech schema");
    for (name, seconds) in [
        ("room-20260906T100000.flac", Some(0.0)),
        ("room-20260906T100100.flac", Some(12.0)),
        ("room-20260906T100200.flac", None),
    ] {
        store::insert(
            &conn,
            &store::Row {
                source: "room".to_owned(),
                filename: name.to_owned(),
                start_utc: "2026-09-06T10:00:00Z".to_owned(),
                bytes: 1,
                sha256: "x".to_owned(),
                received_utc: "2026-09-06T10:00:30Z".to_owned(),
                sent_utc: None,
            },
        )
        .expect("row");
        if let Some(seconds) = seconds {
            conn.execute(
                "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
                 VALUES (?1, 'room', ?2, '2026-09-06T10:01:00Z')",
                (name, seconds),
            )
            .expect("speech row");
        }
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-06T10:30:00Z")
        .expect("t")
        .with_timezone(&chrono::Utc);
    recalld::queue::derive_jobs(&conn, now).expect("derive");
    let mut queued: Vec<String> = conn
        .prepare("SELECT filename FROM jobs ORDER BY filename")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    queued.sort();
    assert_eq!(
        queued,
        vec![
            // speech: queued.
            "room-20260906T100100.flac".to_owned(),
            // unmeasured: queued too — "not looked at yet" is not evidence of
            // silence, and a host without a detector must still do work.
            "room-20260906T100200.flac".to_owned(),
        ],
        "the SILENT segment must not be queued"
    );
}
