use audiod::segmenter::{CaptureConfig, Codec, build_segment_argv, segment_output_pattern};
use std::path::PathBuf;

#[test]
fn default_argv_matches_the_python_segmenter() {
    // The golden shape build_segment_argv produces in capture.py — byte for
    // byte, because the shadow comparison depends on identical encodes.
    let config = CaptureConfig::default();
    let argv = build_segment_argv(&config, "/data/p/p-%Y%m%dT%H%M%S.opus");
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
