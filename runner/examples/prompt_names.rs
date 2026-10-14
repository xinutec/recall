//! What the vocabulary prompt buys: glossary words (speakers' names and
//! vocabulary terms) written right. The truth is people's corrections still in
//! force whose text holds a glossary word. Each clip is transcribed whole, as
//! production does, once per arm: no prompt, the prompt recalld serves (a
//! list), and the same words as a sentence. A word counts as found when the
//! model writes it within a second of the corrected line. Also counted: clips
//! whose detected language an arm changes, a prompt's cost.
//!
//! Prints counts only, never a name, transcript or correction.
//!
//!     cargo run --release -p runner --example prompt_names -- --root LAB [--list]
//!
//! LAB holds `recall.sqlite`, `ingest.sqlite` and `ingest/<source>/<file>`;
//! `--list` prints the clips whose audio it needs, as `<source>/<file>`.
#![expect(
    clippy::cast_precision_loss,
    reason = "seconds into a clip and counts of corrections, far inside f64's exact range"
)]

use clap::Parser;
use runner::shim::Shim;
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use transcript::Act;

recalld::statements! {
    UPLOADS: Meaning = "SELECT id FROM sources WHERE kind = 'upload'";
}

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    root: PathBuf,
    /// Print the clips whose audio is needed, and stop.
    #[arg(long)]
    list: bool,
    /// Uploaded meetings too: hours of audio per clip.
    #[arg(long)]
    uploads: bool,
}

/// One clip and the corrected lines in it: seconds into the clip, and the
/// glossary words each one's text holds.
struct Case {
    source: String,
    filename: String,
    lines: Vec<(f64, f64, Vec<String>)>,
}

fn open(path: PathBuf) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open")
}

fn tokens(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn cases(root: &Path, glossary: &HashSet<String>, uploads: bool) -> Vec<Case> {
    let meaning = open(root.join("recall.sqlite"));
    let ingest = open(root.join("ingest.sqlite"));
    let upload_sources: HashSet<String> = UPLOADS
        .prepare(&meaning)
        .expect("sources")
        .query_map([], |r| r.get(0))
        .expect("sources")
        .collect::<rusqlite::Result<_>>()
        .expect("sources");
    let edits = recalld::edit_log::all(&meaning).expect("edits");
    let gone = transcript::taken_back(&edits);
    let mut by_clip: BTreeMap<i64, Case> = BTreeMap::new();
    for edit in edits.iter().filter(|e| !gone.contains(&e.id)) {
        let Act::Words {
            clip, over, text, ..
        } = &edit.act
        else {
            continue;
        };
        let wanted: Vec<String> = tokens(text.as_str())
            .into_iter()
            .filter(|t| glossary.contains(t))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let Some(c) = recalld::clips::by_id(&ingest, *clip).expect("clip") else {
            continue;
        };
        if !uploads && upload_sources.contains(c.source.as_str()) {
            continue;
        }
        let at = |t: transcript::Instant| (t.micros() - c.start.micros()) as f64 / 1e6;
        by_clip
            .entry(clip.stored())
            .or_insert_with(|| Case {
                source: c.source.as_str().to_owned(),
                filename: c.filename.clone(),
                lines: Vec::new(),
            })
            .lines
            .push((at(over.start()), at(over.end()), wanted));
    }
    by_clip.into_values().collect()
}

/// The model's words, lowercased, with their middle in seconds.
fn heard(reply: &audiocore::shim::asr::Reply) -> Vec<(f64, String)> {
    reply
        .segments
        .iter()
        .flat_map(|s| s.words.iter().flatten())
        .map(|w| (f64::midpoint(w.start, w.end), tokens(&w.text).concat()))
        .collect()
}

#[derive(Default)]
struct Arm {
    found: usize,
    language_changed: usize,
}

fn main() {
    let cli = Cli::parse();
    let meaning = open(cli.root.join("recall.sqlite"));
    let listed = recalld::labels::initial_prompt(&meaning)
        .expect("prompt")
        .expect("a glossary");
    let glossary: HashSet<String> = listed.split(", ").flat_map(tokens).collect();
    let cases = cases(&cli.root, &glossary, cli.uploads);
    if cli.list {
        for c in &cases {
            println!("{}/{}", c.source, c.filename);
        }
        return;
    }
    let terms: Vec<&str> = listed.split(", ").collect();
    let sentence = match terms.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}.", rest.join(", ")),
        _ => format!("{listed}."),
    };
    let arms: [(&str, Option<&str>); 3] = [
        ("no prompt", None),
        ("list (served)", Some(&listed)),
        ("sentence", Some(&sentence)),
    ];

    let python = std::env::var("RECALL_PYTHON").unwrap_or_else(|_| ".venv/bin/python".to_owned());
    let mut shim =
        Shim::spawn(&python, &["-m".to_owned(), "recall.shim_asr".to_owned()]).expect("asr shim");
    assert_eq!(shim.hello().expect("hello"), "asr");

    let mut tally: Vec<Arm> = arms.iter().map(|_| Arm::default()).collect();
    let (mut wanted, mut clips) = (0, 0);
    for case in &cases {
        let audio = cli
            .root
            .join("ingest")
            .join(&case.source)
            .join(&case.filename);
        if !audio.exists() {
            continue;
        }
        clips += 1;
        wanted += case.lines.iter().map(|(_, _, w)| w.len()).sum::<usize>();
        let mut base_language = None;
        for (i, (_, prompt)) in arms.iter().enumerate() {
            let Ok(answer) = shim.transcribe(&audio, None, *prompt) else {
                continue;
            };
            let words = heard(&answer.reply);
            if i == 0 {
                base_language.clone_from(&answer.reply.language);
            } else if answer.reply.language != base_language {
                tally[i].language_changed += 1;
            }
            for (from, to, want) in &case.lines {
                for w in want {
                    let near = |at: f64| at >= from - 1.0 && at <= to + 1.0;
                    if words.iter().any(|(at, t)| near(*at) && t == w) {
                        tally[i].found += 1;
                    }
                }
            }
        }
        eprint!(".");
    }
    println!("\n{clips} clips, {wanted} glossary words in people's corrections");
    for ((name, _), arm) in arms.iter().zip(&tally) {
        println!(
            "  {name:<14} found {:>3} of {wanted} ({:.1}%)  language changed on {} clips",
            arm.found,
            100.0 * arm.found as f64 / wanted.max(1) as f64,
            arm.language_changed
        );
    }
}
