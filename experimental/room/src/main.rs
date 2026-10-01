//! `room`: the room stream by hand, on a local copy of the fleet's data. Never
//! run against production; see `README.md`.

use audiocore::{decode, vad, wav};
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use recalld::store;
use room::RoomConfig;
use room::pieces::{in_block_time, pieces};
use runner::client::Client;
use runner::shim::Shim;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Room experiments over a copy of the fleet's ingest.sqlite. Clips land in
/// ROOT/ingest/<source>/; fetch and transcribe read `RECALL_SYNC_TOKEN`.
#[derive(Parser)]
#[command(name = "room")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch the window's clips from the fleet.
    Fetch {
        #[command(flatten)]
        args: Args,
    },
    /// Build the window's room audio.
    Build {
        #[command(flatten)]
        args: Args,
    },
    /// Transcribe the window.
    Transcribe {
        #[command(flatten)]
        args: Args,
        #[arg(long)]
        out: PathBuf,
        /// What is decoded: the whole minute, its pieces cut at pauses, or
        /// runs of pieces in one language, each decoded whole in that language.
        #[arg(long, value_enum, default_value_t = Arm::Whole)]
        arm: Arm,
        /// The shim and its arguments: everything after it, verbatim [default:
        /// `python -m recall.shim_asr`].
        #[arg(long, num_args = 1.., allow_hyphen_values = true, value_name = "PROGRAM [ARGS]")]
        shim: Vec<String>,
    },
}

/// The window every verb works on.
#[derive(clap::Args)]
struct Args {
    /// Holds a copy of the fleet's ingest.sqlite.
    #[arg(long, value_name = "DIR")]
    root: PathBuf,
    /// An ISO-8601 instant.
    #[arg(long, value_name = "T", value_parser = instant)]
    from: DateTime<Utc>,
    #[arg(long, value_name = "T", value_parser = instant)]
    to: DateTime<Utc>,
    #[arg(long, default_value = "https://recall.xinutec.org")]
    url: String,
}

fn instant(raw: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| format!("not an ISO-8601 instant: {e}"))
}

fn client(url: &str) -> Client {
    let Ok(token) = std::env::var("RECALL_SYNC_TOKEN") else {
        eprintln!("room: RECALL_SYNC_TOKEN must be set");
        std::process::exit(2)
    };
    Client::new(url, &token)
}

/// The window's clips plus a minute either side: a block reads every clip
/// overlapping it, and a clip starts up to a minute before its block.
fn margin(args: &Args) -> (DateTime<Utc>, DateTime<Utc>) {
    let pad = Duration::seconds(room::BLOCK_S);
    (args.from - pad, args.to + pad)
}

fn fetch(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let conn = store::open(&args.root)?;
    let (from, to) = margin(args);
    let mut stmt =
        conn.prepare("SELECT filename, source, start_utc FROM segments WHERE source != ?1")?;
    let rows: Vec<(String, String, String)> = stmt
        .query_map([store::ROOM_SOURCE], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let client = client(&args.url);
    let (mut fetched, mut had) = (0, 0);
    for (filename, source, start) in rows {
        let Ok(start) = DateTime::parse_from_rfc3339(&start) else {
            continue;
        };
        let start = start.with_timezone(&Utc);
        if start < from || start >= to {
            continue;
        }
        let dir = store::source_dir(&args.root, &source);
        let path = dir.join(&filename);
        if path.exists() {
            had += 1;
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        client.fetch_blob(&source, &filename, &path)?;
        fetched += 1;
    }
    println!("fetched {fetched} clip(s); {had} already here");
    Ok(())
}

fn build(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut measured = 0;
    loop {
        let n = room::levels::scan_once(&args.root, 200, Some(margin(args)))?;
        if n == 0 {
            break;
        }
        measured += n;
    }
    // Nothing is still arriving in a copy, so nothing needs to settle.
    let config = RoomConfig {
        settle: Duration::zero(),
        batch: 500,
        window: Some((args.from, args.to)),
        ..RoomConfig::default()
    };
    let mut total = room::BuildSummary::default();
    loop {
        let pass = room::build_once(&args.root, &config, Utc::now())?;
        if pass.built + pass.silent + pass.sparse + pass.gated == 0 {
            total.deferred = pass.deferred;
            break;
        }
        total.built += pass.built;
        total.silent += pass.silent;
        total.sparse += pass.sparse;
        total.gated += pass.gated;
    }
    println!(
        "measured {measured} clip(s); built {} block(s), silent {}, sparse {}, deferred {}",
        total.built, total.silent, total.sparse, total.deferred
    );
    Ok(())
}

/// Blocks and arms already in `out`, so a rerun continues rather than repeats.
fn done_already(out: &Path) -> HashSet<(String, String)> {
    let Ok(file) = std::fs::File::open(out) else {
        return HashSet::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter_map(|v| {
            Some((
                v["block"].as_str()?.to_owned(),
                v["arm"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

fn transcribe_pieces(
    shim: &mut Shim,
    detector: &mut vad::Detector,
    clip: &Path,
    scratch: &Path,
    prompt: Option<&str>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let pcm = decode::decode_s16(clip, vad::RATE).ok_or("cannot decode the block")?;
    let samples = decode::to_f32(&pcm);
    let mut segments = Vec::new();
    for piece in pieces(detector.regions(&samples)?) {
        let from = (piece.start * f64::from(vad::RATE)) as usize;
        let to = ((piece.end * f64::from(vad::RATE)) as usize).min(samples.len());
        if to <= from {
            continue;
        }
        wav::write_mono16(scratch, vad::RATE, &samples[from..to])?;
        let result = shim.transcribe(scratch, None, prompt)?.raw;
        segments.extend(in_block_time(&result, piece.start));
    }
    Ok(json!({ "segments": segments }))
}

/// What `transcribe` decodes for each block.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Arm {
    Whole,
    Pieces,
    Runs,
}

impl Arm {
    const fn name(self) -> &'static str {
        match self {
            Self::Whole => "whole",
            Self::Pieces => "pieces",
            Self::Runs => "runs",
        }
    }
}

/// Each piece decoded once for its language, then runs of one language
/// ([`room::pieces::runs`]) decoded whole with that language forced.
fn transcribe_runs(
    shim: &mut Shim,
    detector: &mut vad::Detector,
    clip: &Path,
    scratch: &Path,
    prompt: Option<&str>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let pcm = decode::decode_s16(clip, vad::RATE).ok_or("cannot decode the block")?;
    let samples = decode::to_f32(&pcm);
    let rate = f64::from(vad::RATE);
    let slice = |start: f64, end: f64| {
        let from = (start * rate) as usize;
        let to = ((end * rate) as usize).min(samples.len());
        &samples[from.min(to)..to]
    };
    let mut languages = Vec::new();
    for piece in pieces(detector.regions(&samples)?) {
        let audio = slice(piece.start, piece.end);
        if audio.is_empty() {
            continue;
        }
        wav::write_mono16(scratch, vad::RATE, audio)?;
        let result = shim.transcribe(scratch, None, prompt)?.raw;
        let language = result["language"].as_str().map(String::from);
        languages.push((piece, language));
    }
    let mut segments = Vec::new();
    for run in room::pieces::runs(&languages, samples.len() as f64 / rate) {
        let audio = slice(run.start, run.end);
        if audio.is_empty() {
            continue;
        }
        wav::write_mono16(scratch, vad::RATE, audio)?;
        let result = shim
            .transcribe_in(scratch, run.language.as_deref(), prompt)?
            .raw;
        segments.extend(in_block_time(&result, run.start));
    }
    Ok(json!({ "segments": segments }))
}

fn transcribe(
    args: &Args,
    out: &Path,
    arm: Arm,
    shim: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    let pieces = arm != Arm::Whole;
    let arm_name = arm.name();
    let conn = store::open(&args.root)?;
    let mut stmt = conn.prepare(
        "SELECT start_utc, winner, filename FROM room_blocks
         WHERE verdict LIKE 'built%' ORDER BY start_utc",
    )?;
    let blocks: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    let done = done_already(out);
    // The vocabulary, as the runner fetches it: production transcribes with it.
    let prompt = client(&args.url).prompt()?;
    let default = ["python", "-m", "recall.shim_asr"].map(String::from);
    let command = if shim.is_empty() { &default[..] } else { shim };
    let (program, shim_args) = (&command[0], &command[1..]);
    let mut shim = Shim::spawn(program, shim_args)?;
    let mut detector = if pieces {
        Some(vad::Detector::load()?)
    } else {
        None
    };
    let scratch = std::env::temp_dir().join(format!("room-piece-{}.wav", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)?;
    let mut written = 0;
    for (start, winner, filename) in blocks {
        let Ok(t) = DateTime::parse_from_rfc3339(&start) else {
            continue;
        };
        let t = t.with_timezone(&Utc);
        if t < args.from || t >= args.to || done.contains(&(start.clone(), arm_name.to_owned())) {
            continue;
        }
        let clip = store::source_dir(&args.root, store::ROOM_SOURCE).join(&filename);
        let result = match (&mut detector, arm) {
            (Some(detector), Arm::Pieces) => {
                transcribe_pieces(&mut shim, detector, &clip, &scratch, prompt.as_deref())?
            }
            (Some(detector), Arm::Runs) => {
                transcribe_runs(&mut shim, detector, &clip, &scratch, prompt.as_deref())?
            }
            _ => shim.transcribe(&clip, None, prompt.as_deref())?.raw,
        };
        let line = json!({ "block": start, "winner": winner, "arm": arm_name, "result": result });
        writeln!(file, "{line}")?;
        written += 1;
    }
    let _ = std::fs::remove_file(&scratch);
    println!("transcribed {written} block(s) as {arm_name}");
    Ok(())
}

/// The decoder is ffmpeg, and a missing one reads as silence rather than an
/// error: every block would be judged `no-audio`, on the record.
fn require_ffmpeg() {
    let found = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        eprintln!("room: ffmpeg is not on PATH; run inside `nix develop`");
        std::process::exit(1);
    }
}

fn main() {
    require_ffmpeg();
    let command = Cli::parse().command;
    let (name, done) = match &command {
        Command::Fetch { args } => ("fetch", fetch(args)),
        Command::Build { args } => ("build", build(args)),
        Command::Transcribe {
            args,
            out,
            arm,
            shim,
        } => ("transcribe", transcribe(args, out, *arm, shim)),
    };
    if let Err(err) = done {
        eprintln!("room {name}: {err}");
        std::process::exit(1);
    }
}
