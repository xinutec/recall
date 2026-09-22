//! The instant feed, through its public surface.
//!
//! The cutting tests run on REAL speech — the committed public-domain reading —
//! because silero is trained on speech and a tone proves nothing about it in
//! either direction. That fixture ships with the repo, so these run in CI, in
//! the nix sandbox and on a fresh clone.

// Sample counts and window counts as numbers. Exact for anything a test can
// reach, and the alternative is `try_from` noise around every assertion.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use audiocore::decode;
use audiocore::vad::{RATE, WINDOW, window_seconds};
use chrono::{DateTime, TimeDelta, Utc};
use runner::live::{Cutter, Utterance, channel, offer, spoken, tap_argv};
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
    for (i, window) in samples.as_chunks::<WINDOW>().0.iter().enumerate() {
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

/// An utterance of `seconds` of (nominal) audio, starting `at` seconds into the
/// epoch. The samples are what makes the length checks below real: a join that
/// stamped a span it had not actually bridged would pass every timestamp
/// assertion and send the model a splice.
fn utterance(at: f64, seconds: f64) -> Utterance {
    Utterance {
        samples: vec![0.1; (seconds * f64::from(RATE)) as usize],
        start: epoch() + TimeDelta::milliseconds((at * 1000.0) as i64),
        end: epoch() + TimeDelta::milliseconds(((at + seconds) * 1000.0) as i64),
    }
}

/// Everything `drain` emits for a fixed set of utterances, with the sender
/// dropped first so the queue is full before the transcriber ever looks — which
/// is what a shim that has fallen behind the microphone produces.
fn batches(utterances: Vec<Utterance>) -> Vec<Utterance> {
    let (to, from) = channel();
    for utterance in utterances {
        assert!(offer(&to, utterance), "the transcriber is alive");
    }
    drop(to);
    let mut out = Vec::new();
    runner::live::drain(&from, |batch| out.push(batch));
    out
}

#[test]
fn utterances_waiting_on_a_busy_shim_go_out_in_one_call() {
    // ⚠ The whole of #1383. A call costs its 30-second window whatever it
    // holds, so four calls for four fragments is four encoder passes for two
    // seconds of speech — and the lag grows for as long as anyone talks.
    let queued = vec![
        utterance(0.0, 0.5),
        utterance(1.0, 0.5),
        utterance(2.0, 0.5),
        utterance(3.0, 0.5),
    ];
    let batches = batches(queued);
    assert_eq!(batches.len(), 1, "four fragments, {} calls", batches.len());
    let batch = &batches[0];
    assert_eq!(batch.start, epoch(), "the batch starts where the burst did");
    assert!(
        (batch.seconds() - 3.5).abs() < 0.01,
        "the batch spans {:.2}s of a 3.5s burst",
        batch.seconds()
    );
}

#[test]
fn the_pause_between_joined_utterances_is_in_the_audio_the_model_hears() {
    // ⚠ Splicing speech end-to-end would hand the model a discontinuity where
    // the room had a breath. The archive pass, which is the better arm, sees a
    // whole 60 s clip with its silence in it; this must not be cheaper than that
    // in a way the model can hear.
    let batches = batches(vec![utterance(0.0, 1.0), utterance(2.0, 1.0)]);
    let batch = batches.first().expect("one call");
    let carried = batch.samples.len() as f64 / f64::from(RATE);
    assert!(
        (carried - batch.seconds()).abs() < 0.01,
        "{carried:.3}s of audio stamped {:.3}s long",
        batch.seconds()
    );
    assert!(
        batch.samples[16_000..32_000].iter().all(|s| *s == 0.0),
        "the bridged second is not the pause that was there"
    );
}

#[test]
fn a_speaker_who_has_stopped_is_not_held_back_for_the_next_one() {
    // Past BRIDGE_SECONDS the sentence is finished. Joining it to whatever comes
    // next would be latency bought for nothing — the call was going to cost the
    // same window either way.
    let apart = runner::live::BRIDGE_SECONDS + 1.0;
    let batches = batches(vec![utterance(0.0, 1.0), utterance(1.0 + apart, 1.0)]);
    assert_eq!(batches.len(), 2, "a long pause was bridged");
    assert!(
        (batches[1].seconds() - 1.0).abs() < 0.01,
        "and the second one survived it, at {:.2}s",
        batches[1].seconds()
    );
}

#[test]
fn no_call_outruns_the_window_it_is_paying_for() {
    // ⚠ Joining without a cap would walk past 30 s, where a SECOND encoder pass
    // appears — the one place the flat cost stops being flat — and would make
    // worst-case latency the length of the conversation.
    let queued: Vec<_> = (0..runner::live::BACKLOG)
        .map(|i| utterance(i as f64 * 3.0, 2.5))
        .collect();
    let spoken = queued.len();
    let batches = batches(queued);
    assert!(
        batches.len() > 1,
        "{spoken} utterances went out in one call"
    );
    for batch in &batches {
        assert!(
            batch.seconds() <= runner::live::CALL_SECONDS,
            "a call carries {:.1}s",
            batch.seconds()
        );
    }
    // ⚠ Nothing may be dropped on the floor by the batching itself: a refused
    // join starts the next call, it does not lose the utterance.
    let total: f64 = batches.iter().map(Utterance::seconds).sum();
    assert!(
        total >= 2.5 * spoken as f64,
        "{total:.1}s of {spoken} survived"
    );
}
