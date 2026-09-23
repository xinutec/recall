//! The ffmpeg argv the capture producers build. Option order matters: a
//! misplaced channel flag makes a mono microphone refuse to open, and
//! `recall-capture` then crash-loops writing header-only files.

use audiod::capture_run::{alsa_argv, sox_argv};

/// Everything before `-i` configures the INPUT; everything after, the output.
fn before_input(argv: &[String]) -> Vec<String> {
    let at = argv.iter().position(|a| a == "-i").expect("an -i");
    argv[..at].to_vec()
}

#[test]
fn the_alsa_input_is_opened_with_the_channel_count_we_want() {
    // `-ac` after `-i` only downmixes what arrived. ffmpeg's ALSA demuxer asks
    // the device for two channels unless `-channels` precedes `-i`, and a
    // mono-only microphone refuses with "cannot set channel count to 2".
    let argv = alsa_argv(Some("hw:CARD=Microphone,DEV=0"), 48_000, 1, None);
    let head = before_input(&argv);
    let at = head
        .iter()
        .position(|a| a == "-channels")
        .expect("the channel count must be an INPUT option");
    assert_eq!(head[at + 1], "1");
}

#[test]
fn the_device_is_still_what_follows_minus_i() {
    // A device on the wrong flag would open ffmpeg's default input and look healthy.
    let argv = alsa_argv(Some("hw:CARD=Microphone,DEV=0"), 48_000, 1, None);
    let at = argv.iter().position(|a| a == "-i").expect("an -i");
    assert_eq!(argv[at + 1], "hw:CARD=Microphone,DEV=0");
}

#[test]
fn a_stereo_source_still_asks_for_two() {
    // The channel count is carried, not hardcoded to mono.
    let argv = alsa_argv(Some("hw:CARD=Other,DEV=0"), 48_000, 2, None);
    let head = before_input(&argv);
    let at = head
        .iter()
        .position(|a| a == "-channels")
        .expect("channels");
    assert_eq!(head[at + 1], "2");
}

#[test]
fn sox_is_untouched_by_this() {
    // The Mac captures through sox, where `-c` is already a device option.
    let argv = sox_argv(Some("USB Condenser Microphone"), 48_000, 1, None);
    let at = argv.iter().position(|a| a == "-c").expect("a -c");
    assert_eq!(argv[at + 1], "1");
}

// ---- the store-and-forward recorder's own heartbeat ----

use audiod::capture_run::beat_body;

#[test]
fn a_store_and_forward_recorder_beats_that_it_is_not_streaming() {
    // Liveness cannot be derived from segments arriving: a recorder delivers
    // nothing both when paused and when its microphone is dead, and those two
    // must not look alike. So it beats on its own.
    let beat = beat_body("geb", true);
    assert_eq!(beat["device"], "geb");
    assert_eq!(beat["app"], "linux");
    assert_eq!(
        beat["streaming"], false,
        "audio reaches the fleet by upload here, never by a live socket"
    );
    assert_eq!(beat["micOk"], true);
    assert!(
        beat["version"].as_str().is_some_and(|v| !v.is_empty()),
        "a beat with no version cannot tell a stale recorder from a current one"
    );
}

#[test]
fn a_producer_that_died_beats_mic_ok_false() {
    // A device that will not open is the fact the beat exists to report;
    // delivery alone says nothing when capture crash-loops.
    let beat = beat_body("geb", false);
    assert_eq!(beat["micOk"], false);
}
