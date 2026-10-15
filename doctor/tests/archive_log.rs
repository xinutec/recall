//! What the archive half reads off disk: who registered, and what they recorded.

use audiocore::capture_log::{self, Event};
use chrono::{DateTime, Duration, TimeZone, Utc};
use doctor::archive::{recorded_intervals, registered_devices};
use doctor::capture::recorders_on_disk;
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

/// A segment name starting `minutes_ago` before now.
fn named(source: &str, minutes_ago: i64) -> String {
    let start = Utc::now() - Duration::minutes(minutes_ago);
    format!("{source}-{}.flac", start.format("%Y%m%dT%H%M%S"))
}

fn start_of(name: &str) -> DateTime<Utc> {
    audiocore::names::parse_segment_start(name).expect("a segment name")
}

#[test]
fn a_segment_covers_from_its_name_to_its_last_write_and_a_stub_covers_nothing() {
    let root = tempfile::tempdir().expect("dir");
    let dir = root.path().join("usb");
    std::fs::create_dir(&dir).expect("source dir");
    let segment = named("usb", 10);
    std::fs::write(dir.join(&segment), b"audio").expect("segment");
    std::fs::write(dir.join(named("usb", 9)), b"").expect("stub");
    std::fs::write(dir.join(".alive"), b"").expect("marker");

    let since: DateTime<Utc> = Utc::now() - Duration::hours(1);
    let intervals = recorded_intervals(root.path(), "usb", since);
    assert_eq!(intervals.len(), 1);
    let (start, end) = intervals[0];
    assert_eq!(start, start_of(&segment));
    assert!(end >= since, "the end is the file's mtime");
}

/// The archive keeps every file; reading each one's metadata on every run is
/// what outran the doctor's bound. A file is selected by its name first.
#[test]
fn a_segment_named_long_before_the_window_is_not_read_even_if_written_since() {
    let root = tempfile::tempdir().expect("dir");
    let dir = root.path().join("usb");
    std::fs::create_dir(&dir).expect("source dir");
    std::fs::write(dir.join(named("usb", 3 * 24 * 60)), b"audio").expect("old name, fresh write");

    let since: DateTime<Utc> = Utc::now() - Duration::hours(1);
    assert!(recorded_intervals(root.path(), "usb", since).is_empty());
}

#[test]
fn a_mic_last_wrote_its_newest_non_empty_segment_by_name() {
    let root = tempfile::tempdir().expect("dir");
    let dir = root.path().join("usb");
    std::fs::create_dir(&dir).expect("source dir");
    let older = named("usb", 30);
    let newest = named("usb", 20);
    std::fs::write(dir.join(&newest), b"audio").expect("newest");
    // Written after the newest, but named earlier: names order the archive.
    std::fs::write(dir.join(&older), b"audio").expect("older");
    std::fs::write(dir.join(named("usb", 10)), b"").expect("a stub after both");
    let newest_written = std::fs::metadata(dir.join(&newest))
        .and_then(|m| m.modified())
        .map(DateTime::<Utc>::from)
        .expect("mtime");

    let recorders = recorders_on_disk(root.path(), &[("usb".to_owned(), SourceKind::CoreAudio)]);

    assert_eq!(recorders[0].last_audio, Some(newest_written));
}
