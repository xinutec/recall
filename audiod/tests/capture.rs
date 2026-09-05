use audiocore::wav::write_mono16;
use audiod::capture_run::{segment_is_digital_silence, sox_argv};
use std::path::Path;

#[test]
fn sox_argv_matches_the_python_producer() {
    // The golden shape sources.py builds for the pinned CoreAudio device.
    assert_eq!(
        sox_argv(Some("USB Condenser Microphone"), 48_000, 1, None),
        [
            "sox",
            "-q",
            "-t",
            "coreaudio",
            "USB Condenser Microphone",
            "-c",
            "1",
            "-r",
            "48000",
            "-b",
            "16",
            "-t",
            "raw",
            "-e",
            "signed-integer",
            "-",
        ]
        .map(String::from)
    );
    // Bounded run appends sox's trim effect.
    assert_eq!(
        sox_argv(None, 48_000, 1, Some(5)).as_slice(),
        [
            "sox",
            "-q",
            "-d",
            "-c",
            "1",
            "-r",
            "48000",
            "-b",
            "16",
            "-t",
            "raw",
            "-e",
            "signed-integer",
            "-",
            "trim",
            "0",
            "5"
        ]
        .map(String::from)
        .as_slice()
    );
}

fn wav_of(dir: &Path, name: &str, samples: &[f32]) -> std::path::PathBuf {
    let path = dir.join(name);
    write_mono16(&path, 16_000, samples).unwrap();
    path
}

#[test]
fn a_wedged_reads_zeros_a_live_room_does_not() {
    // Real decode path (ffmpeg), real files: the watchdog's verdict must hold
    // on what a segmenter actually writes.
    let dir = tempfile::tempdir().unwrap();
    let zeros = wav_of(dir.path(), "dead.wav", &vec![0.0f32; 16_000]);
    assert!(segment_is_digital_silence(&zeros));
    // A quiet room's noise floor (amplitude ~40 of 32768) is NOT silence.
    let quiet: Vec<f32> = (0..16_000)
        .map(|i| {
            if i % 2 == 0 {
                40.0 / 32768.0
            } else {
                -40.0 / 32768.0
            }
        })
        .collect();
    let room = wav_of(dir.path(), "room.wav", &quiet);
    assert!(!segment_is_digital_silence(&room));
}

#[test]
fn an_empty_file_is_silence_and_garbage_is_no_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty.opus");
    std::fs::write(&empty, b"").unwrap();
    assert!(segment_is_digital_silence(&empty));
    // Unreadable is NOT a verdict: never cycle on doubt.
    let garbage = dir.path().join("garbage.opus");
    std::fs::write(&garbage, b"not audio at all").unwrap();
    assert!(!segment_is_digital_silence(&garbage));
}
