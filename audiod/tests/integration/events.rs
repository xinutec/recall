use audiocore::capture_log;

#[test]
fn a_registration_and_an_event_land_in_the_capture_log() {
    let dir = tempfile::tempdir().unwrap();
    audiod::events::register(dir.path(), "pixel9", "tcp_pcm");
    audiod::events::record(dir.path(), audiod::events::INGEST_CONNECT, "pixel9", None);
    let events = capture_log::read(dir.path()).unwrap();
    let seen: Vec<(&str, Option<&str>, Option<&str>)> = events
        .iter()
        .map(|e| (e.kind.as_str(), e.source.as_deref(), e.detail.as_deref()))
        .collect();
    assert_eq!(
        seen,
        [
            (capture_log::REGISTER, Some("pixel9"), Some("tcp_pcm")),
            (capture_log::INGEST_CONNECT, Some("pixel9"), None),
        ]
    );
}

#[test]
fn a_log_that_cannot_be_written_is_swallowed_not_fatal() {
    // Bookkeeping must never take the audio pump down with it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(capture_log::FILE)).unwrap();
    audiod::events::register(dir.path(), "pixel9", "tcp_pcm");
    audiod::events::record(dir.path(), audiod::events::PAUSE, "pixel9", None);
}
