//! A leased job's pinned language reaches the runner's own type.

use runner::client::Job;

#[test]
fn a_leased_language_is_read_and_its_absence_is_the_models_guess() {
    let pinned: Job = serde_json::from_str(
        r#"{"id": 1, "kind": "transcribe-segment", "filename": "m-20261003T100800.ogg",
            "source": "meeting-20261003-1108", "language": "nl"}"#,
    )
    .expect("job");
    assert_eq!(pinned.language.as_deref(), Some("nl"));
    let guessed: Job = serde_json::from_str(
        r#"{"id": 2, "kind": "transcribe-segment", "filename": "usb-20261003T100800.flac",
            "source": "usb"}"#,
    )
    .expect("job");
    assert_eq!(guessed.language, None);
}
