//! What the vocabulary prompt BUYS: household names spelled right (#1665).
//!
//! The other half of `prompt_cost`. That one measures the harm — the prompt
//! putting a name into audio containing none. This measures the benefit it was
//! added for (#1463), on the only ground truth that exists: corrections whose
//! human-written text contains an enrolled name.
//!
//! ⚠ **Both halves are needed before the prompt is dropped for short clips.**
//! Trading a hallucinated name for a misspelled one is not obviously progress,
//! and neither number means anything without the other.
//!
//!     cargo run -p runner --example prompt_spelling -- [<archive root>]
//!
//! ⚠ Prints COUNTS and CLIP LENGTHS only. No name, no transcript and no
//! correction text is ever echoed.

use audiocore::{decode, vad::RATE};
use runner::live::spoken;
use runner::shim::Shim;
use std::path::{Path, PathBuf};

const ROOT: &str = "/Volumes/Backup/recall";

struct Case {
    clip: PathBuf,
    from: f64,
    to: f64,
    /// The names the human's text contains — searched for, never printed.
    wanted: Vec<String>,
}

fn cases(root: &Path) -> Vec<Case> {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("archive");
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM speakers WHERE name <> '' \
             UNION SELECT DISTINCT speaker_label FROM transcript_segments \
             WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%' \
               AND speaker_label <> ''",
        )
        .expect("prepare")
        .query_map([], |r| r.get::<_, String>(0))
        .expect("query")
        .filter_map(Result::ok)
        .collect();

    let mut stmt = conn
        .prepare(
            "SELECT c.corrected_text, a.path, \
                    (julianday(c.start_utc) - julianday(a.start_utc)) * 86400.0, \
                    (julianday(c.end_utc)   - julianday(a.start_utc)) * 86400.0 \
               FROM corrections c JOIN audio_segments a ON a.id = c.audio_segment_id",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })
        .expect("query");

    let mut out = Vec::new();
    for row in rows.filter_map(Result::ok) {
        let (text, path, from, to) = row;
        let lower = text.to_lowercase();
        let wanted: Vec<String> = names
            .iter()
            .filter(|n| lower.contains(&n.trim().to_lowercase()))
            .cloned()
            .collect();
        // ⚠ A span outside its own clip is a clock mismatch, not a case.
        if wanted.is_empty() || from < 0.0 || to <= from {
            continue;
        }
        let clip = root.join(path.trim_start_matches("/data/"));
        if clip.exists() {
            out.push(Case {
                clip,
                from,
                to,
                wanted,
            });
        }
    }
    out
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "example sizes: seconds of audio, far inside range"
)]
fn span(case: &Case) -> Option<Vec<f32>> {
    let pcm = decode::decode_s16(&case.clip, RATE)?;
    let all = decode::to_f32(&pcm);
    let from = (case.from * f64::from(RATE)) as usize;
    let to = ((case.to * f64::from(RATE)) as usize).min(all.len());
    (to > from).then(|| all[from..to].to_vec())
}

fn spelled(shim: &mut Shim, clip: &Path, prompt: Option<&str>, wanted: &[String]) -> bool {
    let Ok(result) = shim.transcribe(clip, None, prompt) else {
        return false;
    };
    spoken(&result).is_some_and(|(text, _)| {
        let lower = text.to_lowercase();
        wanted
            .iter()
            .any(|n| lower.contains(&n.trim().to_lowercase()))
    })
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ROOT.to_owned());
    let cases = cases(Path::new(&root));
    assert!(!cases.is_empty(), "no name-bearing corrections with audio");

    let token = std::env::var("RECALL_SYNC_TOKEN").expect("RECALL_SYNC_TOKEN must be set");
    let client = runner::client::Client::new("http://10.100.0.2:8001", &token);
    let prompt = client
        .prompt()
        .expect("glossary")
        .expect("a non-empty glossary");

    let python = std::env::var("RECALL_PYTHON").unwrap_or_else(|_| ".venv/bin/python".to_owned());
    let mut shim =
        Shim::spawn(&python, &["-m".to_owned(), "recall.shim_asr".to_owned()]).expect("asr shim");
    assert_eq!(shim.hello().expect("hello"), "asr");

    let (mut with, mut without, mut n) = (0, 0, 0);
    let (mut short_with, mut short_without, mut short_n) = (0, 0, 0);
    for case in &cases {
        let Some(samples) = span(case) else { continue };
        let file = tempfile::Builder::new()
            .suffix(".wav")
            .tempfile()
            .expect("scratch");
        audiocore::wav::write_mono16(file.path(), RATE, &samples).expect("write");
        let a = spelled(&mut shim, file.path(), Some(&prompt), &case.wanted);
        let b = spelled(&mut shim, file.path(), None, &case.wanted);
        n += 1;
        with += usize::from(a);
        without += usize::from(b);
        // The clips a "drop the prompt under 2 s" rule would actually touch.
        if case.to - case.from < 2.0 {
            short_n += 1;
            short_with += usize::from(a);
            short_without += usize::from(b);
        }
    }

    println!("\nthe name is present in the transcript (human's text is the truth):");
    println!("  all spans      {with} of {n} WITH prompt    {without} of {n} without");
    println!(
        "  spans < 2s     {short_with} of {short_n} WITH prompt    {short_without} of {short_n} without"
    );
    println!("\n⚠ n is small — this is the whole ground truth that exists, not a sample of it.");
}
