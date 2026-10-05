//! What joining the live tier's calls costs and buys, on real audio.
//!
//! Runs each clip through the real cutter and shim twice: once per utterance,
//! and once joined as `live::drain` joins them when the shim is behind. Prints
//! the wall time and the text of each, since the question is whether a joined
//! call transcribes the same words, not only whether it is cheaper.
//!
//!     cargo run -p runner --example live_cost -- <clip.flac> [more...]
//!
//! With no arguments it uses the committed public-domain reading.
//! `RECALL_PYTHON` overrides the interpreter (default: the project venv, which
//! has the weights; a bare `python` fails inside the shim, which reports it as
//! a refusal).

use audiocore::decode;
use audiocore::vad::{RATE, WINDOW};
use chrono::{TimeDelta, Utc};
use clap::Parser;
use runner::live::{Cutter, Utterance, spoken};
use runner::shim::Shim;
use std::path::Path;
use std::time::Instant;

const FIXTURE: &str = "tests/fixtures/speech/public-domain-en.flac";

/// Cut a file into utterances, the clock advancing one window per window: a
/// tap that drops nothing.
fn cut(path: &Path) -> Vec<Utterance> {
    let pcm = decode::decode_s16(path, RATE).expect("decode");
    let samples = decode::to_f32(&pcm);
    let mut cutter = Cutter::open().expect("detector");
    let epoch = Utc::now();
    let mut out = Vec::new();
    let mut now = epoch;
    for (i, window) in samples.as_chunks::<WINDOW>().0.iter().enumerate() {
        now = epoch + TimeDelta::milliseconds(millis(i + 1));
        out.extend(cutter.feed(window, now).expect("feed"));
    }
    out.extend(cutter.flush(now));
    out
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "example sizes: seconds of audio, far inside range"
)]
fn millis(windows: usize) -> i64 {
    (windows as f64 * audiocore::vad::window_seconds() * 1000.0) as i64
}

/// The utterances joined as `live::drain` joins them when all are waiting.
fn joined(utterances: &[Utterance]) -> Vec<Utterance> {
    let mut out: Vec<Utterance> = Vec::new();
    for utterance in utterances {
        match out.last_mut().map(|batch| batch.join(utterance.clone())) {
            Some(Ok(())) => {}
            Some(Err(refused)) => out.push(refused),
            None => out.push(utterance.clone()),
        }
    }
    out
}

/// Wall time and text of transcribing each clip in turn.
fn run(shim: &mut Shim, clips: &[Utterance]) -> (f64, Vec<String>) {
    let started = Instant::now();
    let mut said = Vec::new();
    for clip in clips {
        let file = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .expect("a scratch clip");
        audiocore::wav::write_mono16(file.path(), RATE, &clip.samples).expect("write");
        let result = shim
            .transcribe(file.path(), None, None)
            .expect("transcribe");
        said.extend(spoken(&result.reply).map(|(text, _)| text));
    }
    (started.elapsed().as_secs_f64(), said)
}

fn report(name: &str, clips: &[Utterance], seconds: f64, said: &[String]) {
    let audio: f64 = clips.iter().map(Utterance::seconds).sum();
    println!(
        "\n{name}: {} calls, {audio:.1}s of audio, {seconds:.2}s of work ({:.2}x real time)",
        clips.len(),
        seconds / audio.max(f64::EPSILON),
    );
    println!("  {}", said.join(" "));
}

/// What joining the live tier's calls costs and buys, on real audio.
#[derive(Parser)]
struct Cli {
    /// Audio to cut and transcribe [default: the public-domain fixture].
    clips: Vec<String>,
}

fn main() {
    let args = Cli::parse().clips;
    let paths: Vec<&Path> = if args.is_empty() {
        vec![Path::new(FIXTURE)]
    } else {
        args.iter().map(Path::new).collect()
    };
    let python = std::env::var("RECALL_PYTHON").unwrap_or_else(|_| ".venv/bin/python".to_owned());
    let mut shim = Shim::spawn(&python, &["-m".to_owned(), "recall.shim_asr".to_owned()])
        .expect("the asr shim");
    assert_eq!(shim.hello().expect("hello"), "asr", "the asr shim");
    for path in paths {
        let utterances = cut(path);
        // A discarded call first: Whisper loads its weights on the first
        // transcribe, not on `hello`, and that cost belongs to neither arm.
        run(&mut shim, &utterances[..1]);
        let batches = joined(&utterances);
        println!("\n=== {} ===", path.display());
        // Fragments first, so any warm-up favours them, not the joined arm.
        let (fragments, fragment_text) = run(&mut shim, &utterances);
        let (joint, batch_text) = run(&mut shim, &batches);
        report("per utterance", &utterances, fragments, &fragment_text);
        report("joined", &batches, joint, &batch_text);
    }
}
