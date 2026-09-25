//! `room`: the room stream by hand, on a local copy of the fleet's data. Never
//! run against production; see `README.md`.

use audiocore::{decode, vad, wav};
use chrono::{DateTime, Duration, Utc};
use recalld::store;
use room::RoomConfig;
use room::pieces::{in_block_time, pieces};
use runner::client::Client;
use runner::shim::Shim;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

fn usage() -> ! {
    eprintln!(
        "usage:
  room fetch      --root DIR --from T --to T [--url URL]
  room build      --root DIR --from T --to T
  room transcribe --root DIR --from T --to T --out FILE [--pieces] [--url URL]
                  [--shim PROGRAM ARGS...]

DIR holds a copy of the fleet's ingest.sqlite; clips land in DIR/ingest/<source>/.
T is an ISO-8601 instant. fetch and transcribe read RECALL_SYNC_TOKEN."
    );
    std::process::exit(2)
}

struct Args {
    root: PathBuf,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    url: String,
    out: Option<PathBuf>,
    pieces: bool,
    shim: (String, Vec<String>),
}

fn instant(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| usage())
        .with_timezone(&Utc)
}

fn parse(mut cli: impl Iterator<Item = String>) -> Args {
    let (mut root, mut from, mut to, mut out) = (None, None, None, None);
    let mut url = "http://10.100.0.2:8001".to_owned();
    let mut pieces = false;
    let mut shim = (
        "python".to_owned(),
        vec!["-m".to_owned(), "recall.shim_asr".to_owned()],
    );
    while let Some(arg) = cli.next() {
        let mut value = || cli.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value())),
            "--from" => from = Some(instant(&value())),
            "--to" => to = Some(instant(&value())),
            "--url" => url = value(),
            "--out" => out = Some(PathBuf::from(value())),
            "--pieces" => pieces = true,
            "--shim" => {
                let program = value();
                shim = (program, cli.by_ref().collect());
            }
            _ => usage(),
        }
    }
    let (Some(root), Some(from), Some(to)) = (root, from, to) else {
        usage()
    };
    Args {
        root,
        from,
        to,
        url,
        out,
        pieces,
        shim,
    }
}

fn client(url: &str) -> Client {
    let Ok(token) = std::env::var("RECALL_SYNC_TOKEN") else {
        usage()
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
        let result = shim.transcribe(scratch, None, prompt)?;
        segments.extend(in_block_time(&result, piece.start));
    }
    Ok(json!({ "segments": segments }))
}

fn transcribe(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let Some(out) = &args.out else { usage() };
    let arm = if args.pieces { "pieces" } else { "whole" };
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
    let (program, shim_args) = &args.shim;
    let mut shim = Shim::spawn(program, shim_args)?;
    let mut detector = if args.pieces {
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
        if t < args.from || t >= args.to || done.contains(&(start.clone(), arm.to_owned())) {
            continue;
        }
        let clip = store::source_dir(&args.root, store::ROOM_SOURCE).join(&filename);
        let result = match &mut detector {
            Some(detector) => {
                transcribe_pieces(&mut shim, detector, &clip, &scratch, prompt.as_deref())?
            }
            None => shim.transcribe(&clip, None, prompt.as_deref())?,
        };
        let line = json!({ "block": start, "winner": winner, "arm": arm, "result": result });
        writeln!(file, "{line}")?;
        written += 1;
    }
    let _ = std::fs::remove_file(&scratch);
    println!("transcribed {written} block(s) as {arm}");
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
    let mut cli = std::env::args().skip(1);
    let command = cli.next().unwrap_or_else(|| usage());
    let args = parse(cli);
    let done = match command.as_str() {
        "fetch" => fetch(&args),
        "build" => build(&args),
        "transcribe" => transcribe(&args),
        _ => usage(),
    };
    if let Err(err) = done {
        eprintln!("room {command}: {err}");
        std::process::exit(1);
    }
}
