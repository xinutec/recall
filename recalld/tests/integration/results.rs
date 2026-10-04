//! A clip's current model output, read by its id.

use audiocore::job::Kind;
use recalld::results::Outcome;
use recalld::store::{self, Row};

#[test]
fn a_clips_current_transcription_is_read_by_its_id_and_a_retranscription_replaces_it() {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = store::open(dir.path()).expect("ingest");
    let name = "usb-20261004T120000.flac";
    store::insert(
        &conn,
        &Row {
            source: "usb".into(),
            filename: name.into(),
            start_utc: "2026-10-04T12:00:00Z".into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-10-04T12:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("segment");
    let clip = recalld::clips::by_filename(&conn, name)
        .unwrap()
        .unwrap()
        .id;
    assert!(
        recalld::results::transcription(&conn, clip)
            .unwrap()
            .is_none()
    );

    let finish = |text: &str| {
        conn.execute(
            "INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
             VALUES (?1, ?2, 'done', '2026-10-04T12:02:00Z', '2026-10-04T12:02:00Z', ?3)
             ON CONFLICT (kind, filename) DO UPDATE SET result = excluded.result",
            (
                Kind::TranscribeSegment,
                name,
                format!(r#"{{"ok":true,"result":{{"language":"nl","segments":[{{"start":0.0,"end":1.0,"text":"{text}"}}]}}}}"#),
            ),
        )
        .expect("result");
    };
    finish("eerste");
    finish("tweede");

    let Some(Outcome::Answer { answer, .. }) =
        recalld::results::transcription(&conn, clip).unwrap()
    else {
        panic!("an answer");
    };
    assert_eq!(
        answer.segments[0].text, "tweede",
        "only the current result is kept"
    );
    assert!(
        recalld::results::diarization(&conn, clip)
            .unwrap()
            .is_none(),
        "a transcription is never read as a diarization"
    );
}

#[test]
fn a_refusal_is_told_apart_from_not_yet_done() {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = store::open(dir.path()).expect("ingest");
    let name = "usb-20261004T120000.flac";
    store::insert(
        &conn,
        &Row {
            source: "usb".into(),
            filename: name.into(),
            start_utc: "2026-10-04T12:00:00Z".into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-10-04T12:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("segment");
    conn.execute(
        r#"INSERT INTO jobs (kind, filename, state, created_utc, done_utc, result)
           VALUES (?1, ?2, 'done', 'x', '2026-10-04T12:02:00Z', '{"ok":false,"error":"decode failed"}')"#,
        (Kind::TranscribeSegment, name),
    )
    .expect("refusal");
    let clip = recalld::clips::by_filename(&conn, name)
        .unwrap()
        .unwrap()
        .id;

    let refused = recalld::results::transcription(&conn, clip).unwrap();
    assert!(matches!(refused, Some(Outcome::Refused { ref why, .. }) if why == "decode failed"));
}
