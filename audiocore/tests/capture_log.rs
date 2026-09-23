//! The capture log: what audiod appends is what the doctor reads.

use audiocore::capture_log::{self, Event};
use chrono::{TimeZone, Utc};

fn event(second: u32, kind: &str, source: Option<&str>) -> Event {
    Event {
        utc: Utc
            .with_ymd_and_hms(2026, 9, 23, 10, 0, second)
            .single()
            .expect("instant"),
        kind: kind.to_owned(),
        source: source.map(str::to_owned),
        detail: None,
    }
}

#[test]
fn appended_events_read_back_in_order() {
    let root = tempfile::tempdir().expect("dir");
    let written = [
        event(0, capture_log::RESUME, Some("usb")),
        event(5, capture_log::PAUSE, None),
    ];
    for e in &written {
        capture_log::append(root.path(), e).expect("append");
    }
    assert_eq!(capture_log::read(root.path()).expect("read"), written);
    let text = std::fs::read_to_string(root.path().join(capture_log::FILE)).expect("file");
    assert_eq!(
        text.lines().next(),
        Some(r#"{"utc":"2026-09-23T10:00:00+00:00","kind":"resume","source":"usb"}"#)
    );
}

#[test]
fn an_absent_log_is_empty_and_a_torn_last_line_is_skipped() {
    let root = tempfile::tempdir().expect("dir");
    assert!(capture_log::read(root.path()).expect("read").is_empty());
    capture_log::append(root.path(), &event(0, capture_log::RESUME, Some("usb"))).expect("append");
    let path = root.path().join(capture_log::FILE);
    let mut text = std::fs::read_to_string(&path).expect("file");
    text.push_str(r#"{"utc":"2026-09-23T10:0"#);
    std::fs::write(&path, text).expect("tear");
    assert_eq!(capture_log::read(root.path()).expect("read").len(), 1);
}
