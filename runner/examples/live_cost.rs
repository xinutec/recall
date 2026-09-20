//! What batching the live tier's calls actually costs and buys, on real audio.
//!
//! The live tier's per-call cost is the 30-SECOND WINDOW Whisper pads every
//! input to, not the audio in it (`live::CALL_SECONDS`). This runs one fixture
//! through the real cutter and the real shim twice — once per utterance, as the
//! tier did before, and once joined the way `live::drain` joins them when the
//! shim is behind — and prints the wall time and the text of each.
//!
//! ⚠ **The text is the point, not only the clock.** Arithmetic already says
//! fewer calls is cheaper. What only a run can say is whether a 12-second call
//! transcribes the same words as the fragments it replaces.
//!
//!     cargo run -p runner --example live_cost -- <clip.flac> [more...]
//!
//! With no arguments it uses the committed public-domain reading. `RECALL_PYTHON`
//! overrides the interpreter; the default is the project venv, which holds the
//! weights.

use audiocore::decode;
use audiocore::vad::{RATE, WINDOW};
use chrono::{TimeDelta, Utc};
use runner::live::{Cutter, Utterance, spoken};
use runner::shim::Shim;
use std::path::Path;
use std::time::Instant;

const FIXTURE: &str = "tests/fixtures/speech/public-domain-en.flac";

/// Cut a file into utterances on a clock that advances one window per window —
/// a tap with no dropped datagrams, which is what a file is.
fn cut(path: &Path) -> Vec<Utterance> {
    let pcm = decode::decode_s16(path, RATE).expect("decode");
    let samples = decode::to_f32(&pcm);
    let mut cutter = Cutter::open().expect("detector");
    let epoch = Utc::now();
    let mut out = Vec::new();
    let mut now = epoch;
    for (i, window) in samples.chunks_exact(WINDOW).enumerate() {
        now = epoch + TimeDelta::milliseconds(millis(i + 1));
        out.extend(cutter.feed(window, now).expect("feed"));
    }
    out.extend(cutter.flush(now));
    out
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn millis(windows: usize) -> i64 {
    (windows as f64 * audiocore::vad::window_seconds() * 1000.0) as i64
}

/// The utterances joined exactly as `live::drain` joins them when everything is
/// already waiting — the state a shim that has fallen behind produces.
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

/// Transcribe each clip in turn, returning the wall time and what was said.
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
        said.extend(spoken(&result).map(|(text, _)| text));
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<&Path> = if args.is_empty() {
        vec![Path::new(FIXTURE)]
    } else {
        args.iter().map(Path::new).collect()
    };
    // ⚠ The project venv, not the devshell's python: the weights live there and
    // a bare `python` answers `ModuleNotFoundError` from inside the shim, which
    // arrives as a REFUSAL — i.e. as though the clip were the problem.
    let python = std::env::var("RECALL_PYTHON").unwrap_or_else(|_| ".venv/bin/python".to_owned());
    let mut shim = Shim::spawn(&python, &["-m".to_owned(), "recall.shim_asr".to_owned()])
        .expect("the asr shim");
    assert_eq!(shim.hello().expect("hello"), "asr", "the asr shim");
    for path in paths {
        let utterances = cut(path);
        // ⚠ One discarded call first. Whisper loads its weights on the FIRST
        // transcribe, not on `hello`, so without this the whole model load is
        // charged to whichever arm runs first — and that is the arm under test.
        run(&mut shim, &utterances[..1]);
        let batches = joined(&utterances);
        println!("\n=== {} ===", path.display());
        // ⚠ Fragments FIRST, so the batched arm cannot be the one that benefits
        // from anything the other warmed up.
        let (fragments, fragment_text) = run(&mut shim, &utterances);
        let (joint, batch_text) = run(&mut shim, &batches);
        report("per utterance", &utterances, fragments, &fragment_text);
        report("joined", &batches, joint, &batch_text);
    }
}
