//! The instant feed, through its public surface.
//!
//! The cutting tests run on REAL speech — the committed public-domain reading —
//! because silero is trained on speech and a tone proves nothing about it in
//! either direction. That fixture ships with the repo, so these run in CI, in
//! the nix sandbox and on a fresh clone.

// Sample counts and window counts as numbers. Exact for anything a test can
// reach, and the alternative is `try_from` noise around every assertion.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use audiocore::decode;
use audiocore::vad::{RATE, WINDOW, window_seconds};
use chrono::{DateTime, TimeDelta, Utc};
use runner::live::{Cutter, Utterance, channel, offer, recall_instant, spoken, tap_argv};
use std::path::Path;

const FIXTURE: &str = "../tests/fixtures/speech/public-domain-en.flac";

fn epoch() -> DateTime<Utc> {
    "2026-09-14T09:00:00+00:00".parse().expect("a fixed clock")
}

/// Feed a whole file through the cutter on a synthetic clock that advances by
/// exactly one window per window — what a tap with no dropped datagrams looks
/// like.
fn cut(path: &Path) -> Vec<Utterance> {
    let pcm = decode::decode_s16(path, RATE).expect("decode");
    let samples = decode::to_f32(&pcm);
    let mut cutter = Cutter::open().expect("detector");
    let mut out = Vec::new();
    let mut now = epoch();
    for (i, window) in samples.chunks_exact(WINDOW).enumerate() {
        now = epoch() + window_delta(i + 1);
        if let Some(utterance) = cutter.feed(window, now).expect("feed") {
            out.push(utterance);
        }
    }
    out.extend(cutter.flush(now));
    out
}

fn window_delta(windows: usize) -> TimeDelta {
    TimeDelta::milliseconds((windows as f64 * window_seconds() * 1000.0) as i64)
}

#[test]
fn real_speech_is_cut_into_utterances_that_carry_their_own_audio() {
    let path = Path::new(FIXTURE);
    assert!(path.exists(), "the committed fixture must not vanish");
    let utterances = cut(path);
    // 48 s of read poetry with real pauses between stanzas. One utterance would
    // mean the pause rule never fired; dozens would mean it fires mid-word.
    assert!(
        (2..=40).contains(&utterances.len()),
        "{} utterances from the reading",
        utterances.len()
    );
    for utterance in &utterances {
        let seconds = utterance.samples.len() as f64 / f64::from(RATE);
        let span = (utterance.end - utterance.start).as_seconds_f64();
        assert!(
            (seconds - span).abs() < window_seconds() * 2.0,
            "an utterance carrying {seconds:.2}s of audio is stamped {span:.2}s long"
        );
        assert!(seconds >= 0.25, "a region shorter than the minimum got out");
    }
}

#[test]
fn an_utterance_is_stamped_where_it_was_said() {
    // ⚠ The whole point of deriving the stamp BACKWARDS from `now`. On a clock
    // that advanced one window per window, the answer must match the offset
    // into the file — and on a tap that dropped datagrams, it still would,
    // which counting forwards from an anchor cannot do.
    let utterances = cut(Path::new(FIXTURE));
    let first = utterances.first().expect("the reading is not silent");
    let into_file = (first.start - epoch()).as_seconds_f64();
    // The reading opens within the first few seconds; what is being pinned is
    // that the stamp tracks the FILE rather than the moment of transcription.
    assert!(
        (0.0..8.0).contains(&into_file),
        "the first utterance is stamped {into_file:.2}s into the reading"
    );
    for pair in utterances.windows(2) {
        assert!(
            pair[0].end <= pair[1].start,
            "utterances overlap: {} then {}",
            pair[0].end,
            pair[1].start
        );
    }
}

#[test]
fn silence_produces_nothing() {
    let mut cutter = Cutter::open().expect("detector");
    let quiet = [0.0_f32; WINDOW];
    let mut now = epoch();
    for i in 0..300 {
        now = epoch() + window_delta(i + 1);
        assert_eq!(cutter.feed(&quiet, now).expect("feed"), None);
    }
    assert_eq!(cutter.flush(now), None);
}

#[test]
fn the_tap_is_read_from_the_socket_audiod_publishes_on() {
    // ⚠ Pinned as ARGV, in order, because ffmpeg reads a flag's meaning from
    // which side of `-i` it sits on. `-ac 1` after the input is an OUTPUT
    // channel count and leaves the demuxer on its two-channel default — the
    // exact mistake that had geb recording stubs on 2026-09-13.
    let argv = tap_argv(runner::live::TAP);
    let i = argv.iter().position(|a| a == "-i").expect("an input");
    let before = &argv[..i];
    assert!(before.contains(&"s16le".to_owned()), "input format");
    assert!(before.contains(&"16000".to_owned()), "input rate");
    assert!(before.contains(&"1".to_owned()), "input channels");
    assert!(
        argv[i + 1].starts_with("udp://127.0.0.1:9876?"),
        "the tap is {}",
        argv[i + 1]
    );
    assert!(argv[i + 1].contains("overrun_nonfatal=1"), "droppable");
    assert_eq!(argv.last().expect("an output"), "-", "PCM to stdout");
}

#[test]
fn the_shim_reply_becomes_one_line_of_text() {
    let reply = serde_json::json!({
        "language": "nl",
        "segments": [
            {"start": 0.0, "end": 1.0, "text": " Goedemorgen "},
            {"start": 1.0, "end": 2.0, "text": "allemaal."},
        ],
    });
    assert_eq!(
        spoken(&reply),
        Some(("Goedemorgen allemaal.".to_owned(), Some("nl".to_owned())))
    );
}

#[test]
fn a_reply_with_nothing_said_in_it_is_not_a_turn() {
    // silero heard a voice and the model found no words in it. Ordinary, not a
    // fault — and pushing an empty turn would put a blank line on the timeline.
    for reply in [
        serde_json::json!({"language": "en", "segments": []}),
        serde_json::json!({"language": "en", "segments": [{"text": "   "}]}),
        serde_json::json!({"language": "en"}),
    ] {
        assert_eq!(spoken(&reply), None, "{reply}");
    }
}

#[test]
fn an_instant_is_spelled_the_way_the_rest_of_the_system_stores_one() {
    // python's `datetime.isoformat()`: microseconds, a numeric offset, no `Z`.
    // A second spelling of one instant is how two rows for one turn happen.
    let at: DateTime<Utc> = "2026-09-14T09:00:00.123456+00:00".parse().expect("parse");
    assert_eq!(recall_instant(at), "2026-09-14T09:00:00.123456+00:00");
    let whole: DateTime<Utc> = "2026-09-14T09:00:00+00:00".parse().expect("parse");
    assert_eq!(recall_instant(whole), "2026-09-14T09:00:00.000000+00:00");
}

#[test]
fn a_backed_up_transcriber_drops_rather_than_blocks() {
    // ⚠ Blocking here would stall the READER, which loses the same audio a
    // window later and the reader's clock with it. The archive pass transcribes
    // this minute properly regardless, so dropping costs a few seconds of feed.
    let (to, from) = channel();
    let utterance = Utterance {
        samples: vec![0.0; WINDOW],
        start: epoch(),
        end: epoch(),
    };
    for _ in 0..64 {
        assert!(
            offer(&to, utterance.clone()), // far past the channel's depth
            "a full queue is not a reason to stop reading"
        );
    }
    let received = std::iter::from_fn(|| from.try_recv().ok()).count();
    assert!(
        received > 0 && received < 64,
        "{received} of 64 got through"
    );
    // A GONE transcriber is: every later utterance would be lost too, so the
    // reader must stop and let KeepAlive restart the pair.
    drop(from);
    assert!(!offer(&to, utterance));
}
