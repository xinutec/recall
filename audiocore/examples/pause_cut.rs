//! Cut a clip at its pauses and write each piece, to measure what the minute
//! grid costs (#1388).
//!
//! The room builder cuts on the UTC minute and that is deliberate: it
//! reproduces the instrument the WER bake-off measured, hard cuts included.
//! But a minute is not a unit of speech, and a block that straddles two
//! languages gets ONE language label for both — which collapses whichever is
//! the minority. Pieces cut at pauses should each hold one language.
//!
//! This writes the pieces; transcribing them and comparing is the caller's job,
//! because the comparison needs the ASR shim and this crate must not.
//!
//! ```text
//! cargo run -p audiocore --example pause_cut -- <clip> <outdir>
//! ```
//!
//! ⚠ Prints spans and durations only, never a filename that could name a
//! household source beyond what the caller already passed in.

use audiocore::{decode, vad, wav};
use std::path::PathBuf;

/// A pause shorter than this is within an utterance, not between two. Matched
/// to the live tier's bridge, which is the only pause length this system has
/// measured anything about.
const JOIN_PAUSE_S: f64 = 2.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [clip, outdir] = args.as_slice() else {
        eprintln!("usage: pause_cut <clip> <outdir>");
        std::process::exit(2);
    };
    let clip = PathBuf::from(clip);
    let out = PathBuf::from(outdir);
    std::fs::create_dir_all(&out).expect("outdir");

    let pcm = decode::decode_s16(&clip, vad::RATE).expect("decode");
    let samples = decode::to_f32(&pcm);
    let total = samples.len() as f64 / f64::from(vad::RATE);

    let mut detector = vad::Detector::load().expect("silero");
    let regions = detector.regions(&samples).expect("regions");

    // Join regions separated by less than a real pause: the question is where
    // the speaker STOPPED, not where the detector blinked.
    let mut pieces: Vec<vad::Region> = Vec::new();
    for r in regions {
        match pieces.last_mut() {
            Some(last) if r.start - last.end < JOIN_PAUSE_S => last.end = r.end,
            _ => pieces.push(r),
        }
    }

    println!("clip {total:.1}s -> {} piece(s)", pieces.len());
    for (i, piece) in pieces.iter().enumerate() {
        let from = (piece.start * f64::from(vad::RATE)) as usize;
        let to = ((piece.end * f64::from(vad::RATE)) as usize).min(samples.len());
        if to <= from {
            continue;
        }
        let path = out.join(format!("piece{i:02}.wav"));
        wav::write_mono16(&path, vad::RATE, &samples[from..to]).expect("write");
        println!(
            "  piece{i:02}  {:>6.1}s .. {:>6.1}s  ({:.1}s)",
            piece.start,
            piece.end,
            piece.seconds()
        );
    }
}
