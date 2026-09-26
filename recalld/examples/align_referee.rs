//! The production alignment over stored diarize and transcribe results (#1711).
//!
//! Reads `pyannote.jsonl` from `scripts/identify_referee.py extract-pyannote`
//! and writes, per clip, the aligned turns and each cluster's voiceprint, which
//! `identify_referee.py score` names and scores. The parsing and the alignment
//! are recalld's own, so the control is what production would have shown.
//!
//! Usage: `cargo run --example align_referee -- <work dir>`

use recalld::align::{MIN_TURN_S, assign_words_to_speakers};
use recalld::diarized::{voices, words_of};
use std::io::{BufRead, Write};

fn main() {
    let work = std::path::PathBuf::from(std::env::args().nth(1).expect("work dir"));
    let input = std::fs::File::open(work.join("pyannote.jsonl")).expect("pyannote.jsonl");
    let mut out = std::fs::File::create(work.join("aligned.jsonl")).expect("aligned.jsonl");
    for line in std::io::BufReader::new(input).lines() {
        let record: serde_json::Value = serde_json::from_str(&line.expect("line")).expect("json");
        let (Some((words, _)), Some((speakers, prints))) = (
            words_of(&record["transcribe"].to_string(), None),
            voices(&record["diarize"].to_string()),
        ) else {
            continue;
        };
        let turns: Vec<serde_json::Value> = assign_words_to_speakers(&words, &speakers, MIN_TURN_S)
            .iter()
            .map(|t| serde_json::json!({"start": t.start, "end": t.end, "speaker": t.speaker}))
            .collect();
        let voices: Vec<serde_json::Value> = prints
            .iter()
            .map(|p| serde_json::json!({"speaker": p.speaker, "vector": p.vector}))
            .collect();
        let row =
            serde_json::json!({"audio_id": record["audio_id"], "turns": turns, "voices": voices});
        writeln!(out, "{row}").expect("write");
    }
}
