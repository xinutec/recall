use audiod::segmenter::{CaptureConfig, Codec, build_segment_argv, segment_output_pattern};
use std::path::PathBuf;

/// The default is the retention decision, and it is the thing an archive can
/// never take back: a source that takes the default and records lossy has lost
/// the phase information for good. Measured 2026-09-11, that is not
/// hypothetical — every phone segment in the archive was `.opus` because
/// `audiod ingest` took a lossy default while the fleet had already decided on
/// lossless.
#[test]
fn the_default_is_lossless_because_a_wrong_default_cannot_be_taken_back() {
    let config = CaptureConfig::default();

    assert_eq!(config.codec, Codec::Flac);
    assert_eq!(config.bitrate, None);
    assert_eq!(config.codec.container_ext(), "flac");
}

/// Each codec owns its bitrate so the pair cannot disagree: ffmpeg ACCEPTS
/// `-b:a 32k` next to `-c:a flac` and ignores it, so a mismatched pair looks
/// configured and is silently meaningless.
#[test]
fn a_lossless_codec_asks_for_no_bitrate_and_a_lossy_one_does() {
    assert_eq!(Codec::Flac.default_bitrate(), None);
    assert_eq!(Codec::PcmS16le.default_bitrate(), None);
    assert_eq!(Codec::Libopus.default_bitrate(), Some("32k"));
    assert_eq!(Codec::Aac.default_bitrate(), Some("32k"));
}

#[test]
fn opus_argv_matches_the_python_segmenter() {
    // The golden shape build_segment_argv produces in capture.py — byte for
    // byte, because the shadow comparison depends on identical encodes. Opus is
    // named explicitly now that it is no longer the default; the parity this
    // pins is the Opus encode's, and it outlives which codec is default.
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

/// ⚠ **The condenser is the mic any fusion wants most, and it was the one being
/// thrown through a perceptual codec.** Opus at 32 kbps is transparent to an ear
/// and destructive to PHASE — it codes what you notice, not the waveform, and
/// fills some bands with synthesised noise. Two Opus streams of one room
/// therefore cannot be summed coherently: their fine structure was altered
/// independently by two encoders. Lossless is the prerequisite for combining
/// microphones at all, and for keeping the audio itself worth listening to.
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
    // The rate is the whole point: 48 kHz kept, not resampled down.
    let rate = argv.iter().position(|a| a == "-ar").expect("-ar");
    assert_eq!(argv[rate + 1], "48000");
}

/// The extension must follow the codec, or the archive's naming contract and
/// the file disagree — `audiocore::names` parses the extension to decide how to
/// decode, so a flac written as `.opus` is unreadable to everything downstream.
#[test]
fn the_container_extension_follows_the_codec() {
    assert_eq!(Codec::Flac.container_ext(), "flac");
    assert_eq!(Codec::Libopus.container_ext(), "opus");
    assert_eq!(
        segment_output_pattern(&PathBuf::from("/data"), "usb", Codec::Flac.container_ext()),
        "/data/usb/usb-%Y%m%dT%H%M%S.flac"
    );
}
