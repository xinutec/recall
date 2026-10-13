//! How often a voiceprint names a voice right, scored on what people named.
//! For each naming that teaches a print (`labels::labelled`), the diarized
//! voice under most of its span is matched (`match_one`, as the view guesses)
//! against every print not learnt from the same clip: leave one clip out, so
//! a voice is never matched against itself.
//!
//! Prints names: keep its output out of tracked files.
//!
//! Usage: `cargo run --release --example naming_census -- --root COPY`

use clap::Parser;
use recalld::identify::{Voiceprint, match_one};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

recalld::statements! {
    CLIP_ACTS: Meaning = "SELECT id, clip_id, origin FROM edits WHERE clip_id IS NOT NULL";
    PRINTS: Meaning =
        "SELECT s.name, e.vector, e.source_edit_id, e.source_segment_id, e.source_correction_id
         FROM speaker_embeddings e JOIN speakers s ON s.id = e.speaker_id";
    SOURCE_KINDS: Meaning = "SELECT id, kind FROM sources";
}

#[derive(Parser)]
struct Cli {
    /// A copy's data root, holding `recall.sqlite` and `ingest.sqlite`.
    #[arg(long)]
    root: PathBuf,
}

fn open(path: PathBuf) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open")
}

/// Every print, with the clip it was learnt from when that can be told: by
/// its naming act, or for a recovered act by the line or correction it names.
fn prints(meaning: &Connection) -> Vec<(Voiceprint, Option<i64>)> {
    let mut act_clip: HashMap<i64, i64> = HashMap::new();
    let mut origin_clip: HashMap<String, i64> = HashMap::new();
    let mut stmt = CLIP_ACTS.prepare(meaning).expect("edits");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .expect("edits");
    for row in rows {
        let (id, clip, origin) = row.expect("edit");
        act_clip.insert(id, clip);
        if let Some(origin) = origin {
            origin_clip.insert(origin, clip);
        }
    }
    let mut stmt = PRINTS.prepare(meaning).expect("prints");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .expect("prints");
    let mut out = Vec::new();
    for row in rows {
        let (person, raw, act, line, correction) = row.expect("print");
        let Ok(vector) = serde_json::from_str::<Vec<f64>>(&raw) else {
            continue;
        };
        let clip = act
            .and_then(|a| act_clip.get(&a).copied())
            .or_else(|| line.and_then(|l| origin_clip.get(&format!("Line({l})")).copied()))
            .or_else(|| {
                correction.and_then(|c| origin_clip.get(&format!("Correction({c})")).copied())
            });
        out.push((Voiceprint { person, vector }, clip));
    }
    out
}

/// The median, to three places; `-` for none.
fn median(mut v: Vec<f64>) -> String {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2)
        .map_or_else(|| "-".to_owned(), |m| format!("{m:.3}"))
}

fn main() {
    let cli = Cli::parse();
    let meaning = open(cli.root.join("recall.sqlite"));
    let ingest = open(cli.root.join("ingest.sqlite"));
    let all = prints(&meaning);
    let unplaced = all.iter().filter(|(_, c)| c.is_none()).count();
    let clips: HashMap<i64, transcript::Clip> = recalld::clips::all(&ingest)
        .expect("clips")
        .into_iter()
        .map(|c| (c.id.stored(), c))
        .collect();

    let kinds: HashMap<String, String> = SOURCE_KINDS
        .prepare(&meaning)
        .expect("sources")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("sources")
        .collect::<rusqlite::Result<_>>()
        .expect("sources");

    let mut skipped: BTreeMap<&str, usize> = BTreeMap::new();
    let mut scored: Vec<Scored> = Vec::new();
    for l in recalld::labels::labelled(&meaning).expect("labelled") {
        let Some(clip) = clips.get(&l.clip.stored()) else {
            *skipped.entry("clip gone").or_default() += 1;
            continue;
        };
        let voices = match recalld::rendering::facts(&ingest, clip) {
            Ok(facts) => facts.voices,
            Err(_) => None,
        };
        let Some(voices) = voices else {
            *skipped.entry("not diarized").or_default() += 1;
            continue;
        };
        let offset = |at: transcript::Instant| (at.micros() - clip.start.micros()) as f64 / 1e6;
        let (from, to) = (offset(l.span.start()), offset(l.span.end()));
        let mut overlap: HashMap<&str, f64> = HashMap::new();
        for t in &voices.turns {
            let o = t.end.min(to) - t.start.max(from);
            if o > 0.0 {
                *overlap.entry(t.speaker.as_str()).or_default() += o;
            }
        }
        let Some((cluster, _)) = overlap.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)) else {
            *skipped.entry("no voice under the span").or_default() += 1;
            continue;
        };
        let Some((_, print)) = voices.prints.iter().find(|(c, _)| c == cluster) else {
            *skipped.entry("voice has no print").or_default() += 1;
            continue;
        };
        let others: Vec<Voiceprint> = all
            .iter()
            .filter(|(_, c)| *c != Some(clip.id.stored()))
            .map(|(p, _)| p.clone())
            .collect();
        let Some(guess) = match_one(print, &others) else {
            *skipped.entry("no guess").or_default() += 1;
            continue;
        };
        let named = l.name.as_str().to_owned();
        scored.push(Scored {
            voice: (clip.id.stored(), cluster.to_owned()),
            known: others.iter().any(|p| p.person == named),
            upload: kinds.get(clip.source.as_str()).map(String::as_str) == Some("upload"),
            named,
            guessed: guess.person,
            score: guess.score,
        });
    }

    println!(
        "prints {} ({} not placeable in a clip, never left out)",
        all.len(),
        unplaced
    );
    println!("skipped {skipped:?}");
    let pick = |upload: bool, known: bool| -> Vec<&Scored> {
        scored
            .iter()
            .filter(|s| s.upload == upload && s.known == known)
            .collect()
    };
    report("microphones, person enrolled elsewhere", &pick(false, true));
    report("uploads, person enrolled elsewhere", &pick(true, true));
    report(
        "microphones, person enrolled nowhere else",
        &pick(false, false),
    );
    report("uploads, person enrolled nowhere else", &pick(true, false));
    merged(&scored);
}

/// A voice people named as two different persons is two speakers the
/// diarizer merged: no print can name both halves right.
fn merged(scored: &[Scored]) {
    let mut names: HashMap<&(i64, String), std::collections::BTreeSet<&str>> = HashMap::new();
    for s in scored {
        names.entry(&s.voice).or_default().insert(&s.named);
    }
    for upload in [false, true] {
        let wrong: Vec<&Scored> = scored
            .iter()
            .filter(|s| s.upload == upload && s.known && s.named != s.guessed)
            .collect();
        let merged = wrong.iter().filter(|s| names[&s.voice].len() > 1).count();
        let voices: std::collections::BTreeSet<&(i64, String)> = scored
            .iter()
            .filter(|s| s.upload == upload)
            .map(|s| &s.voice)
            .collect();
        let mixed = voices.iter().filter(|v| names[*v].len() > 1).count();
        println!(
            "\n{}: {mixed} of {} named voices carry two or more people; {merged} of {} wrong guesses sit on one",
            if upload { "uploads" } else { "microphones" },
            voices.len(),
            wrong.len()
        );
    }
}

/// One naming, scored.
struct Scored {
    /// The clip and the diarized voice under the naming.
    voice: (i64, String),
    named: String,
    guessed: String,
    score: f64,
    /// From an uploaded recording, not a microphone of the house.
    upload: bool,
    /// The named person has a print outside this clip, so a right guess is
    /// possible at all.
    known: bool,
}

fn report(title: &str, scored: &[&Scored]) {
    let right: Vec<f64> = scored
        .iter()
        .filter(|s| s.named == s.guessed)
        .map(|s| s.score)
        .collect();
    let wrong: Vec<f64> = scored
        .iter()
        .filter(|s| s.named != s.guessed)
        .map(|s| s.score)
        .collect();
    println!("\n== {title}");
    println!(
        "scored {}: right {} ({:.1}%), wrong {}",
        scored.len(),
        right.len(),
        100.0 * right.len() as f64 / scored.len().max(1) as f64,
        wrong.len()
    );
    if scored.is_empty() {
        return;
    }
    println!(
        "median score: right {}, wrong {}",
        median(right.clone()),
        median(wrong.clone())
    );
    println!("threshold  right kept  wrong kept");
    for t in [0.35, 0.4, 0.5] {
        let kept = |v: &[f64]| {
            100.0 * v.iter().filter(|s| **s >= t).count() as f64 / v.len().max(1) as f64
        };
        println!(
            "  >= {t:.2}    {:5.1}%     {:5.1}%",
            kept(&right),
            kept(&wrong)
        );
    }
    let mut per_person: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut confusions: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for s in scored {
        let e = per_person.entry(&s.named).or_default();
        e.0 += 1;
        if s.named == s.guessed {
            e.1 += 1;
        } else {
            *confusions.entry((&s.named, &s.guessed)).or_default() += 1;
        }
    }
    println!("named as         n  right");
    for (who, (n, ok)) in &per_person {
        println!("  {who:<16} {n:>4}  {ok:>4}");
    }
    let mut pairs: Vec<_> = confusions.into_iter().collect();
    pairs.sort_by_key(|p| std::cmp::Reverse(p.1));
    println!("wrong: named -> guessed");
    for ((named, guessed), n) in pairs {
        println!("  {named} -> {guessed}: {n}");
    }
}
