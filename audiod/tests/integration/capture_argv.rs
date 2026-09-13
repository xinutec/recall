//! The ffmpeg argv the capture producers build.
//!
//! ⚠ **Untested until 2026-09-13, and that is how geb broke.** Swapping its
//! stereo conference speakerphone for a mono capsule made `recall-capture`
//! crash-loop: four restarts, four header-only stub files, no audio. The cause
//! was option ORDER, which no test looked at.

use audiod::capture_run::{alsa_argv, sox_argv};

/// Everything before `-i` configures the INPUT; everything after, the output.
fn before_input(argv: &[String]) -> Vec<String> {
    let at = argv.iter().position(|a| a == "-i").expect("an -i");
    argv[..at].to_vec()
}

#[test]
fn the_alsa_input_is_opened_with_the_channel_count_we_want() {
    // ⚠ `-ac` AFTER `-i` downmixes what arrived; it does not tell the demuxer
    // what to ask the device for. ffmpeg's ALSA demuxer defaults to TWO, and a
    // mono-only microphone refuses:
    //
    //     [in#0] cannot set channel count to 2 (Invalid argument)
    //
    // Verified on geb: the same command with `-channels 1` before `-i` returns
    // 95,988 bytes where the old form returns 0.
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
    // The ordering fix must not shift the device onto the wrong flag — that
    // would open ffmpeg's default input while looking entirely healthy.
    let argv = alsa_argv(Some("hw:CARD=Microphone,DEV=0"), 48_000, 1, None);
    let at = argv.iter().position(|a| a == "-i").expect("an -i");
    assert_eq!(argv[at + 1], "hw:CARD=Microphone,DEV=0");
}

#[test]
fn a_stereo_source_still_asks_for_two() {
    // The count is carried, not hardcoded: the Mac's condenser is not geb's
    // capsule, and a fix for one must not pin the other to mono.
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
    // The Mac captures through sox, where `-c` is a device option already and
    // the ALSA demuxer's default never applied. Pinned so the fix stays scoped.
    let argv = sox_argv(Some("USB Condenser Microphone"), 48_000, 1, None);
    let at = argv.iter().position(|a| a == "-c").expect("a -c");
    assert_eq!(argv[at + 1], "1");
}
