//! `playback build | play | score`: see the README.

use audiocore::instant;
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use playback::corpus::{self, Utterance};
use playback::plan::{self, PartSpec, Plan, Rng, Voices};
use playback::score::{self, Line, Played};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write DIR/plan.json and one WAV per part from a parts spec.
    Build {
        /// A JSON list of parts: name, device, seconds, voices.
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        dir: PathBuf,
        /// `LibriSpeech`'s split directory (holding `<speaker>/<chapter>/`).
        #[arg(long)]
        librispeech: Option<PathBuf>,
        /// FLEURS, as `<lang>/test.tsv` and `<lang>/test/*.wav`.
        #[arg(long)]
        fleurs: Option<PathBuf>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Play DIR's parts through their devices, appending to DIR/played.jsonl.
    /// Capture is not touched: resume it first, pause it after.
    Play {
        #[arg(long)]
        dir: PathBuf,
        /// Seconds of silence before the first part and after each.
        #[arg(long, default_value_t = 45)]
        gap: u64,
        /// Silence prepended to each part: the Mac ramps its output up over
        /// about half a second when playback starts, and the first turn's own
        /// gap (0.4 s or more) follows it. The logged start skips it.
        #[arg(long, default_value_t = 0.5)]
        lead: f64,
        #[arg(long, default_value = "sox")]
        sox: String,
    },
    /// Score every microphone's lines in a copy of recall.sqlite against DIR's plan.
    Score {
        #[arg(long)]
        dir: PathBuf,
        /// A read-only copy of the fleet's recall.sqlite, never the live file.
        #[arg(long)]
        db: PathBuf,
        /// Seconds before the first part and after the last that count as silence.
        #[arg(long, default_value_t = 40)]
        margin: i64,
        /// Only lines the household is shown, rather than every machine line.
        #[arg(long)]
        shown: bool,
        #[arg(long)]
        json: bool,
    },
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn pools(
    voices: &Voices,
    librispeech: Option<&Path>,
    fleurs: Option<&Path>,
) -> Result<Vec<Vec<Utterance>>> {
    Ok(match voices {
        Voices::Librispeech(speakers) => {
            let root = librispeech.ok_or("a part asks for LibriSpeech: pass --librispeech")?;
            speakers
                .iter()
                .map(|s| corpus::librispeech(root, s))
                .collect::<std::io::Result<_>>()?
        }
        Voices::Fleurs(lang) => {
            let root = fleurs
                .ok_or("a part asks for FLEURS: pass --fleurs")?
                .join(lang);
            vec![corpus::fleurs(
                &root.join("test.tsv"),
                &root.join("test"),
                lang,
            )?]
        }
    })
}

fn decode(u: &Utterance) -> Option<Vec<i16>> {
    let bytes = audiocore::decode::decode_s16(&u.audio, plan::RATE)?;
    Some(
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect(),
    )
}

fn build(
    spec: &Path,
    dir: &Path,
    librispeech: Option<&Path>,
    fleurs: Option<&Path>,
    seed: u64,
) -> Result<()> {
    let specs: Vec<PartSpec> = serde_json::from_slice(&std::fs::read(spec)?)?;
    std::fs::create_dir_all(dir)?;
    let mut rng = Rng::new(seed);
    let mut parts = Vec::new();
    for spec in &specs {
        let mut pools = pools(&spec.voices, librispeech, fleurs)?;
        for pool in &mut pools {
            pool.sort_by(|a, b| a.audio.cmp(&b.audio));
            rng.shuffle(pool);
        }
        let (part, samples) = plan::lay_out(spec, &mut pools, &mut rng, decode)?;
        std::fs::write(dir.join(format!("{}.wav", part.name)), plan::wav(&samples))?;
        println!(
            "{}  {}  {:.0} s, {} turns",
            part.name,
            part.device,
            part.seconds,
            part.turns.len()
        );
        parts.push(part);
    }
    std::fs::write(
        dir.join("plan.json"),
        serde_json::to_vec_pretty(&Plan { seed, parts })?,
    )?;
    Ok(())
}

fn read_plan(dir: &Path) -> Result<Plan> {
    Ok(serde_json::from_slice(&std::fs::read(
        dir.join("plan.json"),
    )?)?)
}

fn play(dir: &Path, gap: u64, lead: f64, sox: &str) -> Result<()> {
    let plan = read_plan(dir)?;
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("played.jsonl"))?;
    let pause = std::time::Duration::from_secs(gap);
    std::thread::sleep(pause);
    for part in &plan.parts {
        let start = Utc::now() + score::seconds(lead);
        let status = std::process::Command::new(sox)
            .arg("-q")
            .arg(dir.join(format!("{}.wav", part.name)))
            .args([
                "-t",
                "coreaudio",
                &part.device,
                "pad",
                &lead.to_string(),
                "0",
            ])
            .status()?;
        if !status.success() {
            return Err(format!("{}: sox exited {status}", part.name).into());
        }
        let played = Played {
            part: part.name.clone(),
            device: part.device.clone(),
            start,
        };
        writeln!(log, "{}", serde_json::to_string(&played)?)?;
        println!(
            "{}  {}  from {}",
            part.name,
            part.device,
            instant::python_isoformat_utc(start)
        );
        std::thread::sleep(pause);
    }
    Ok(())
}

fn read_played(dir: &Path) -> Result<Vec<Played>> {
    let file = std::fs::File::open(dir.join("played.jsonl"))?;
    let mut out = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            out.push(serde_json::from_str(&line)?);
        }
    }
    Ok(out)
}

fn lines(db: &Path, from: DateTime<Utc>, to: DateTime<Utc>, shown: bool) -> Result<Vec<Line>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let sql = format!(
        "SELECT a.source_id, t.start_utc, t.end_utc, t.text
           FROM transcript_segments t JOIN audio_segments a ON a.id = t.audio_segment_id
          WHERE t.superseded_by IS NULL AND t.asr_model != 'human'
            AND t.end_utc > ?1 AND t.start_utc < ?2 {}",
        if shown {
            "AND t.hidden_reason IS NULL"
        } else {
            ""
        }
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        [
            instant::python_isoformat_utc(from),
            instant::python_isoformat_utc(to),
        ],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        },
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (source, start, end, text) = row?;
        let (Some(start), Some(end)) = (instant::parse_utc(&start), instant::parse_utc(&end))
        else {
            return Err(format!("an unreadable instant: {start} .. {end}").into());
        };
        out.push(Line {
            source,
            start,
            end,
            text,
        });
    }
    Ok(out)
}

fn pct(rate: Option<f64>) -> String {
    rate.map_or_else(|| "-".into(), |r| format!("{:.1}%", r * 100.0))
}

fn score(dir: &Path, db: &Path, margin: i64, shown: bool, json: bool) -> Result<()> {
    let plan = read_plan(dir)?;
    let played = read_played(dir)?;
    let first = played
        .iter()
        .map(|p| p.start)
        .min()
        .ok_or("nothing in played.jsonl")?;
    let last = played
        .iter()
        .filter_map(|p| score::span(&plan, p).map(|(_, end)| end))
        .max()
        .ok_or("no played part is in the plan")?;
    let (from, to) = (
        first - Duration::seconds(margin),
        last + Duration::seconds(margin),
    );
    let lines = lines(db, from, to, shown)?;
    let report = score::score(&plan, &played, &lines, from, to);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let names: Vec<&str> = played.iter().map(|p| p.part.as_str()).collect();
    print!(
        "{:12} {:>7} {:>6} {:>6} {:>6}",
        "source", "WER", "sub", "del", "ins"
    );
    for n in &names {
        print!(" {n:>8}");
    }
    println!(" {:>9}", "invented");
    for inv in &report.invented {
        let t = score::total(&report, &inv.source);
        print!(
            "{:12} {:>7} {:>6} {:>6} {:>6}",
            inv.source,
            pct(t.rate()),
            t.substitutions,
            t.deletions,
            t.insertions
        );
        for n in &names {
            let e = report
                .parts
                .iter()
                .find(|p| p.source == inv.source && p.part == *n);
            print!(" {:>8}", pct(e.and_then(|p| p.errors.rate())));
        }
        println!(" {:>9}", inv.words);
    }
    println!(
        "invented: words in lines outside every part, over {:.0} s with nothing played",
        report.silent_seconds
    );
    Ok(())
}

fn main() {
    let done = match Cli::parse().command {
        Command::Build {
            spec,
            dir,
            librispeech,
            fleurs,
            seed,
        } => build(&spec, &dir, librispeech.as_deref(), fleurs.as_deref(), seed),
        Command::Play {
            dir,
            gap,
            lead,
            sox,
        } => play(&dir, gap, lead, &sox),
        Command::Score {
            dir,
            db,
            margin,
            shown,
            json,
        } => score(&dir, &db, margin, shown, json),
    };
    if let Err(err) = done {
        eprintln!("playback: {err}");
        std::process::exit(1);
    }
}
