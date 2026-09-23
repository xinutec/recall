use audiod::segmenter::{CaptureConfig, Codec, build_segment_argv, segment_output_pattern};
use std::path::PathBuf;

/// The default codec is a retention decision: a source that records lossy by
/// default has lost the phase information for good.
#[test]
fn the_default_is_lossless_because_a_wrong_default_cannot_be_taken_back() {
    let config = CaptureConfig::default();

    assert_eq!(config.codec, Codec::Flac);
    assert_eq!(config.bitrate, None);
    assert_eq!(config.codec.container_ext(), "flac");
}

/// Each codec owns its bitrate so the pair cannot disagree: ffmpeg accepts and
/// ignores `-b:a 32k` next to `-c:a flac`, so a mismatch looks configured.
#[test]
fn a_lossless_codec_asks_for_no_bitrate_and_a_lossy_one_does() {
    assert_eq!(Codec::Flac.default_bitrate(), None);
    assert_eq!(Codec::PcmS16le.default_bitrate(), None);
    assert_eq!(Codec::Libopus.default_bitrate(), Some("32k"));
    assert_eq!(Codec::Aac.default_bitrate(), Some("32k"));
}

#[test]
fn opus_argv_matches_the_python_segmenter() {
    // The golden Opus argv, byte for byte. Opus is named explicitly because it
    // is not the default.
    let config = CaptureConfig {
        codec: Codec::Libopus,
        bitrate: Codec::Libopus.default_bitrate().map(Into::into),
        ..CaptureConfig::default()
    };
    let argv = build_segment_argv(&config, "/data/p/p-%Y%m%dT%H%M%S.opus", false);
    assert_eq!(
        argv,
        [
            "-hide_banner",
            "-loglevel",
            "warning",
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ac",
            "1",
            "-i",
            "-",
            "-c:a",
            "libopus",
            "-b:a",
            "32k",
            "-application",
            "voip",
            "-f",
            "segment",
            "-segment_time",
            "60",
            "-reset_timestamps",
            "1",
            "-strftime",
            "1",
            "/data/p/p-%Y%m%dT%H%M%S.opus",
        ]
        .map(String::from)
    );
}

#[test]
fn each_codec_names_its_container() {
    assert_eq!(Codec::Libopus.container_ext(), "opus");
    assert_eq!(Codec::Flac.container_ext(), "flac");
    assert_eq!(Codec::PcmS16le.container_ext(), "wav");
    assert_eq!(Codec::Aac.container_ext(), "m4a");
}

#[test]
fn the_pattern_places_segments_under_the_source_directory() {
    let pattern = segment_output_pattern(&PathBuf::from("/data"), "pixel9", "opus");
    assert_eq!(pattern, "/data/pixel9/pixel9-%Y%m%dT%H%M%S.opus");
}

/// Opus at 32 kbps is transparent to the ear but destroys phase, so two Opus
/// streams of one room cannot be summed coherently. Lossless is the
/// prerequisite for combining microphones.
#[test]
fn a_lossless_capture_writes_flac_and_asks_for_no_bitrate() {
    let config = CaptureConfig {
        codec: Codec::Flac,
        bitrate: None,
        ..CaptureConfig::default()
    };

    let argv = build_segment_argv(&config, "/data/usb/usb-%Y%m%dT%H%M%S.flac", false);

    assert!(argv.contains(&"flac".to_owned()), "{argv:?}");
    assert!(
        !argv.contains(&"-b:a".to_owned()),
        "a bitrate is meaningless for a lossless codec: {argv:?}"
    );
    assert!(
        !argv.contains(&"-application".to_owned()),
        "voip is an Opus setting and must not follow the codec change: {argv:?}"
    );
    // 48 kHz is kept, not resampled down.
    let rate = argv.iter().position(|a| a == "-ar").expect("-ar");
    assert_eq!(argv[rate + 1], "48000");
}

/// The extension must follow the codec: `audiocore::names` parses it to decide
/// how to decode, so a flac named `.opus` is unreadable downstream.
#[test]
fn the_container_extension_follows_the_codec() {
    assert_eq!(Codec::Flac.container_ext(), "flac");
    assert_eq!(Codec::Libopus.container_ext(), "opus");
    assert_eq!(
        segment_output_pattern(&PathBuf::from("/data"), "usb", Codec::Flac.container_ext()),
        "/data/usb/usb-%Y%m%dT%H%M%S.flac"
    );
}

/// The live tap's contract. `recall-live` reads this socket instead of opening
/// the microphone, because two `CoreAudio` clients on one device starve each
/// other, so these argv positions are the interface between recorder and live feed.
#[test]
fn the_live_tap_is_the_second_output_at_the_format_live_reads() {
    let argv = build_segment_argv(
        &CaptureConfig::default(),
        "/data/usb/usb-%Y%m%dT%H%M%S.flac",
        true,
    );
    let pattern = argv
        .iter()
        .position(|a| a.contains("usb-%Y"))
        .expect("the segment output");
    let udp = argv
        .iter()
        .position(|a| a.starts_with("udp://"))
        .expect("the live tap");

    // Second, after the segments: the archive must never wait on a droppable output.
    assert!(udp > pattern, "the tap must follow the segment output");
    assert!(
        argv[udp].contains(":9876"),
        "the tap is {} — runner::live::TAP is the other end",
        argv[udp]
    );
    // The format live decodes without resampling, as output options.
    assert_eq!(
        &argv[udp - 6..udp],
        ["-ar", "16000", "-ac", "1", "-f", "s16le"]
    );
}

#[test]
fn without_the_tap_there_is_no_udp_output_at_all() {
    let argv = build_segment_argv(
        &CaptureConfig::default(),
        "/data/usb/usb-%Y%m%dT%H%M%S.flac",
        false,
    );
    assert!(!argv.iter().any(|a| a.starts_with("udp://")));
}
