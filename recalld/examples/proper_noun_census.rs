//! How often today's transcriptions get a proper noun right, scored on what
//! people typed. For each person's words still in force, the capitalised
//! words they wrote (not opening a sentence) and every glossary word are
//! looked for in the model's current words over the line they replaced:
//! found, spelled otherwise (within two edits), or absent.
//!
//! Prints counts only. `--terms FILE` writes the per-term table, which holds
//! names: keep it out of tracked files.
//!
//! Usage: `cargo run --release --example proper_noun_census -- --root COPY [--terms FILE]`

use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use transcript::Act;
use transcript::render::Heard;

recalld::statements! {
    SPEAKERS: Meaning = "SELECT name FROM speakers";
    VOCABULARY: Meaning = "SELECT term FROM vocabulary";
}

#[derive(Parser)]
struct Cli {
    /// A copy's data root, holding `recall.sqlite` and `ingest.sqlite`.
    #[arg(long)]
    root: PathBuf,
    /// Where to write the per-term table.
    #[arg(long)]
    terms: Option<PathBuf>,
}

fn open(path: PathBuf) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open")
}

/// Words, each with whether it opens a sentence.
fn words(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut opens = true;
    for raw in text.split_whitespace() {
        let word: String = raw.trim_matches(|c: char| !c.is_alphanumeric()).to_owned();
        if !word.is_empty() {
            out.push((word, opens));
        }
        opens = raw.ends_with(['.', '?', '!']);
    }
    out
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let here = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(here + 1);
            prev = here;
        }
    }
    row[b.len()]
}

#[derive(Default)]
struct Tally {
    found: usize,
    respelled: usize,
    absent: usize,
}

impl Tally {
    fn n(&self) -> usize {
        self.found + self.respelled + self.absent
    }
}

/// Every word of the speakers' names and the vocabulary, lowercased.
fn glossary(meaning: &Connection) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in [SPEAKERS, VOCABULARY] {
        let terms: Vec<String> = stmt
            .prepare(meaning)
            .expect("glossary")
            .query_map([], |r| r.get(0))
            .expect("glossary")
            .collect::<rusqlite::Result<_>>()
            .expect("glossary");
        for term in terms {
            out.extend(words(&term).into_iter().map(|(w, _)| w.to_lowercase()));
        }
    }
    out
}

/// The model's words whose middle falls in `from..to` seconds, lowercased.
fn model_words(heard: &Heard, from: f64, to: f64) -> Vec<String> {
    let inside = |a: f64, b: f64| (from..to).contains(&f64::midpoint(a, b));
    let mut text = String::new();
    for s in &heard.segments {
        if let Some(ws) = &s.words {
            for w in ws.iter().filter(|w| inside(w.start, w.end)) {
                text.push_str(&w.text);
            }
        } else if inside(s.start, s.end) {
            let _ = write!(text, " {}", s.text);
        }
    }
    words(&text)
        .into_iter()
        .map(|(w, _)| w.to_lowercase())
        .collect()
}

fn main() {
    let cli = Cli::parse();
    let meaning = open(cli.root.join("recall.sqlite"));
    let ingest = open(cli.root.join("ingest.sqlite"));
    let glossary = glossary(&meaning);
    let clips: HashMap<i64, transcript::Clip> = recalld::clips::all(&ingest)
        .expect("clips")
        .into_iter()
        .map(|c| (c.id.stored(), c))
        .collect();
    let edits = recalld::edit_log::all(&meaning).expect("edits");
    let gone = transcript::taken_back(&edits);

    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    let mut terms: BTreeMap<String, Tally> = BTreeMap::new();
    let mut corrections = 0;
    for edit in edits.iter().filter(|e| !gone.contains(&e.id)) {
        let Act::Words {
            clip, over, text, ..
        } = &edit.act
        else {
            continue;
        };
        let Some(clip) = clips.get(&clip.stored()) else {
            *skipped.entry("clip gone").or_default() += 1;
            continue;
        };
        let heard = recalld::rendering::facts(&ingest, clip)
            .ok()
            .and_then(|f| f.heard);
        let Some(heard) = heard else {
            *skipped.entry("not transcribed").or_default() += 1;
            continue;
        };
        let at = |t: transcript::Instant| (t.micros() - clip.start.micros()) as f64 / 1e6;
        let said = model_words(&heard, at(over.start()), at(over.end()));
        corrections += 1;
        for (word, opens) in words(text.as_str()) {
            let lower = word.to_lowercase();
            let named = word.chars().next().is_some_and(char::is_uppercase)
                && !opens
                && word.chars().count() > 1;
            if !named && !glossary.contains(&lower) {
                continue;
            }
            let tally = terms.entry(lower.clone()).or_default();
            if said.contains(&lower) {
                tally.found += 1;
            } else if said.iter().any(|s| distance(s, &lower) <= 2) {
                tally.respelled += 1;
            } else {
                tally.absent += 1;
            }
        }
    }
    println!("corrections scored {corrections}, skipped {skipped:?}");
    report(&terms, &glossary, cli.terms);
}

fn report(terms: &BTreeMap<String, Tally>, glossary: &HashSet<String>, file: Option<PathBuf>) {
    println!(
        "glossary words {} (speakers' names and vocabulary terms)",
        glossary.len()
    );
    for (title, in_glossary) in [("in the glossary", true), ("not in the glossary", false)] {
        let mut sum = Tally::default();
        let mut distinct = 0;
        for (term, t) in terms {
            if glossary.contains(term) == in_glossary {
                distinct += 1;
                sum.found += t.found;
                sum.respelled += t.respelled;
                sum.absent += t.absent;
            }
        }
        let pct = |x: usize| 100.0 * x as f64 / sum.n().max(1) as f64;
        println!(
            "{title}: {} uses of {distinct} words: found {:.1}%, spelled otherwise {:.1}%, absent {:.1}%",
            sum.n(),
            pct(sum.found),
            pct(sum.respelled),
            pct(sum.absent)
        );
    }
    if let Some(path) = file {
        let mut rows: Vec<(&String, &Tally)> = terms.iter().collect();
        rows.sort_by_key(|(_, t)| std::cmp::Reverse(t.respelled + t.absent));
        let mut out = String::from("term\tuses\tfound\trespelled\tabsent\tglossary\n");
        for (term, t) in rows {
            let _ = writeln!(
                out,
                "{term}\t{}\t{}\t{}\t{}\t{}",
                t.n(),
                t.found,
                t.respelled,
                t.absent,
                glossary.contains(term)
            );
        }
        std::fs::write(path, out).expect("terms");
    }
}
