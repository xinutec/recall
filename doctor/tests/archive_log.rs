//! What the archive half reads off disk: who registered, and what they recorded.

use audiocore::capture_log::{self, Event};
use chrono::{DateTime, Duration, TimeZone, Utc};
use doctor::archive::{recorded_intervals, registered_devices};
use doctor::source::SourceKind;

fn register(minute: u32, source: &str, kind: &str) -> Event {
    Event {
        utc: Utc
            .with_ymd_and_hms(2026, 9, 23, 10, minute, 0)
            .single()
            .expect("instant"),
        kind: capture_log::REGISTER.to_owned(),
        source: Some(source.to_owned()),
        detail: Some(kind.to_owned()),
    }
}

#[test]
fn a_device_is_known_by_its_latest_registration_and_a_meeting_is_not_a_device() {
    let log = [
        register(0, "usb", "tcp_pcm"),
        register(1, "usb", "coreaudio"),
        register(2, "pixel5", "tcp_pcm"),
        register(3, "meeting-x", "upload"),
        register(4, "geb", "alsa"),
    ];
    assert_eq!(
        registered_devices(&log),
        [
            ("pixel5".to_owned(), SourceKind::TcpPcm),
            ("usb".to_owned(), SourceKind::CoreAudio),
        ]
    );
}

#[test]
fn a_segment_covers_from_its_name_to_its_last_write_and_a_stub_covers_nothing() {
    let root = tempfile::tempdir().expect("dir");
    let dir = root.path().join("usb");
    std::fs::create_dir(&dir).expect("source dir");
    std::fs::write(dir.join("usb-20260923T100000.flac"), b"audio").expect("segment");
    std::fs::write(dir.join("usb-20260923T100100.flac"), b"").expect("stub");
    std::fs::write(dir.join(".alive"), b"").expect("marker");

    let since: DateTime<Utc> = Utc::now() - Duration::hours(1);
    let intervals = recorded_intervals(root.path(), "usb", since);
    assert_eq!(intervals.len(), 1);
    let (start, end) = intervals[0];
    assert_eq!(
        start,
        Utc.with_ymd_and_hms(2026, 9, 23, 10, 0, 0)
            .single()
            .expect("instant")
    );
    assert!(end >= since, "the end is the file's mtime");
}
