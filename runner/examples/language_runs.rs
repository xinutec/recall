//! One microphone's minutes decoded whole and in language runs (`runner::runs`),
//! timed, written for `playback score --room` (#1388).
//!
//! Reads a lab directory as `room fetch` leaves it: `DIR/ingest/<source>/` holds
//! the clips and `DIR/ingest.sqlite` the fleet's speech regions for them. Each
//! clip is decoded both ways through the real shim and the real code path; one
//! JSON line per clip and arm (`<source>-whole`, `<source>-runs`), segment
//! times relative to the clip's start.
//!
//!     cargo run -p runner --example language_runs -- --dir DIR --source geb \
//!         --out mic.jsonl --shim <ml-env python> -m recall.shim_asr

use clap::Parser;
use runner::shim::Shim;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    dir: PathBuf,
    #[arg(long)]
    source: String,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, num_args = 1.., allow_hyphen_values = true, required = true)]
    shim: Vec<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let conn = rusqlite::Connection::open_with_flags(
        args.dir.join("ingest.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut shim = Shim::spawn(&args.shim[0], &args.shim[1..])?;
    let scratch = std::env::temp_dir().join(format!("language-runs-{}.wav", std::process::id()));
    let mut out = std::fs::File::create(&args.out)?;
    let mut clips: Vec<PathBuf> = std::fs::read_dir(args.dir.join("ingest").join(&args.source))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    clips.sort();
    let (mut whole_time, mut runs_time) = (Duration::ZERO, Duration::ZERO);
    for clip in &clips {
        let name = clip
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let Some(start) = audiocore::names::parse_segment_start(name) else {
            continue;
        };
        let regions: Option<String> = conn
            .query_row(
                "SELECT regions FROM segment_speech WHERE filename = ?1",
                [name],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let regions: Vec<[f64; 2]> = match regions {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        };
        if regions.is_empty() {
            continue;
        }
        let block = start.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let t = Instant::now();
        let whole = shim.transcribe(clip, None, None)?.raw;
        whole_time += t.elapsed();
        let t = Instant::now();
        let (runs, _) = runner::runs::transcribe(&mut shim, clip, &regions, &scratch, None)?;
        runs_time += t.elapsed();
        for (arm, result) in [("whole", whole), ("runs", runs)] {
            let line = json!({ "block": block, "winner": args.source, "arm": format!("{}-{arm}", args.source), "result": result });
            writeln!(out, "{line}")?;
        }
    }
    let _ = std::fs::remove_file(&scratch);
    println!(
        "{} clips: whole {:.0} s, runs {:.0} s ({:.2}x)",
        clips.len(),
        whole_time.as_secs_f64(),
        runs_time.as_secs_f64(),
        runs_time.as_secs_f64() / whole_time.as_secs_f64().max(f64::EPSILON)
    );
    Ok(())
}
