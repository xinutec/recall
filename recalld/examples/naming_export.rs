//! The naming census's rows, for measuring other ways of naming a voice: each
//! naming's clip, file and span, who was named, and what the per-voice guess
//! (`match_one` on the diarized voice under most of the span, leaving the
//! clip's own prints out) said; what the transcribed segment under most of the
//! span said by its own print (`embed-lines`, the same leave-out), with its
//! margin, or nothing when the clip or segment has no print, and how many
//! diarized voices that segment overlaps; and every
//! voiceprint with the clip it came from. Writes `namings.tsv` and
//! `prints.json` to `--out`.
//!
//! Writes names: keep its output out of tracked files.
//!
//! Usage: `cargo run --release --example naming_export -- --root COPY --out DIR`

use clap::Parser;
use recalld::identify::{Voiceprint, match_one};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
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
    #[arg(long)]
    out: PathBuf,
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

/// What the transcribed segment under most of `from..to` says by its own
/// print: name and margin (empty without a print), and how many diarized
/// voices it overlaps.
fn segment_column(
    heard: Option<&transcript::render::Heard>,
    voices: &transcript::render::Voices,
    others: &[Voiceprint],
    from: f64,
    to: f64,
) -> (String, String, usize) {
    let Some((i, segment)) = heard.and_then(|heard| {
        heard
            .segments
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s, s.end.min(to) - s.start.max(from)))
            .filter(|(_, _, o)| *o > 0.0)
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(i, s, _)| (i, s))
    }) else {
        return (String::new(), String::new(), 0);
    };
    let count = voices
        .turns
        .iter()
        .filter(|t| t.end.min(segment.end) > t.start.max(segment.start))
        .map(|t| t.speaker.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let (person, margin) = voices
        .segment_prints
        .iter()
        .find(|(s, _)| *s == i)
        .and_then(|(_, print)| match_one(print, others))
        .map_or((String::new(), String::new()), |g| {
            (g.person, format!("{:.4}", g.margin))
        });
    (person, margin, count)
}

fn main() {
    use std::io::Write;
    let cli = Cli::parse();
    let meaning = open(cli.root.join("recall.sqlite"));
    let ingest = open(cli.root.join("ingest.sqlite"));
    let all = prints(&meaning);
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
    std::fs::create_dir_all(&cli.out).expect("out");
    let printed: Vec<serde_json::Value> = all
        .iter()
        .map(|(p, c)| serde_json::json!({"person": p.person, "clip": c, "vector": p.vector}))
        .collect();
    std::fs::write(
        cli.out.join("prints.json"),
        serde_json::to_string(&printed).expect("json"),
    )
    .expect("prints");
    let mut tsv = std::fs::File::create(cli.out.join("namings.tsv")).expect("tsv");
    writeln!(
        tsv,
        "clip\tsource\tpath\tfrom\tto\tnamed\tguessed\tscore\tupload\tcluster\tembedded\tsegment_guess\tsegment_margin\tsegment_voices"
    )
    .expect("tsv");
    let mut written = 0;
    for l in recalld::labels::labelled(&meaning).expect("labelled") {
        let Some(clip) = clips.get(&l.clip.stored()) else {
            continue;
        };
        let Ok(facts) = recalld::rendering::facts(&ingest, clip) else {
            continue;
        };
        let Some(voices) = facts.voices else { continue };
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
            continue;
        };
        let Some((_, print)) = voices.prints.iter().find(|(c, _)| c == cluster) else {
            continue;
        };
        let others: Vec<Voiceprint> = all
            .iter()
            .filter(|(_, c)| *c != Some(clip.id.stored()))
            .map(|(p, _)| p.clone())
            .collect();
        let Some(guess) = match_one(print, &others) else {
            continue;
        };
        let (segment_person, segment_margin, segment_voices) =
            segment_column(facts.heard.as_ref(), &voices, &others, from, to);
        let upload = kinds.get(clip.source.as_str()).map(String::as_str) == Some("upload");
        writeln!(
            tsv,
            "{}\t{}\t{}\t{from:.3}\t{to:.3}\t{}\t{}\t{:.4}\t{}\t{cluster}\t{}\t{segment_person}\t{segment_margin}\t{segment_voices}",
            clip.id.stored(),
            clip.source.as_str(),
            clip.path,
            l.name.as_str(),
            guess.person,
            guess.score,
            u8::from(upload),
            u8::from(!voices.segment_prints.is_empty())
        )
        .expect("tsv");
        written += 1;
    }
    println!("{} prints, {written} namings", all.len());
}
