//! `playback build | play | score`: see the README.

use audiocore::instant;
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use playback::corpus::{self, Utterance};
use playback::diarization;
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
    /// Score the fleet's diarization of the played window against who spoke.
    Diarization {
        #[arg(long)]
        dir: PathBuf,
        /// The window's `diarize-segment` jobs, a JSON array of
        /// `{"filename", "source", "result"}` (sqlite3 -json over ingest.sqlite).
        #[arg(long)]
        results: PathBuf,
        /// Trim each reference turn to where its source recording has speech
        /// (the fleet's detector on the clean audio): a recording's leading
        /// silence and inner pauses are not speech a diarizer should cover.
        #[arg(long)]
        vad: bool,
    },
    /// Score a meeting transcript against an AMI word reference, placing every
    /// dropped word: in time no segment covers, or inside one (#1470).
    Meeting {
        /// The corpus's `words/` directory.
        #[arg(long)]
        words: PathBuf,
        /// The meeting id, e.g. `ES2004a`.
        #[arg(long)]
        meeting: String,
        /// Transcripts to score: a Whisper result (`{"segments": [..]}`, bare
        /// or as the shim's reply), or JSON lines of `{start, end, text}`;
        /// times from the recording's start. Repeatable.
        hypothesis: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
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
        Voices::Mix(sources) => plan::interleave(
            sources
                .iter()
                .map(|v| pools(v, librispeech, fleurs))
                .collect::<Result<_>>()?,
        ),
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

/// The lines the household is shown. A hidden line is not an extra hearing:
/// diarization hides the line it rewrote, and the rewrite is shown, so counting
/// both would score the same speech twice.
fn lines(db: &Path, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Line>> {
    let conn =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT a.source_id, t.start_utc, t.end_utc, t.text
           FROM transcript_segments t JOIN audio_segments a ON a.id = t.audio_segment_id
          WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL AND t.asr_model != 'human'
            AND t.end_utc > ?1 AND t.start_utc < ?2",
    )?;
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

/// A hypothesis file: a Whisper result, or JSON lines of segments.
fn read_segments(path: &Path) -> Result<Vec<playback::meeting::Segment>> {
    let text = std::fs::read_to_string(path)?;
    if let Ok(segments) = playback::meeting::whisper_segments(&text) {
        return Ok(segments);
    }
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

fn meeting_score(words: &Path, meeting: &str, hypotheses: &[PathBuf], json: bool) -> Result<()> {
    let mut reference = Vec::new();
    for speaker in ["A", "B", "C", "D", "E"] {
        let path = words.join(format!("{meeting}.{speaker}.words.xml"));
        if let Ok(xml) = std::fs::read_to_string(&path) {
            reference.extend(playback::meeting::ami_words(speaker, &xml));
        }
    }
    if reference.is_empty() {
        return Err(format!("no words for {meeting} in {}", words.display()).into());
    }
    let mut reports = Vec::new();
    for path in hypotheses {
        let report = playback::meeting::score(&reference, &read_segments(path)?);
        reports.push((path.display().to_string(), report));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }
    println!(
        "{meeting}: {} reference words, {} speakers (fillers and broken-off words left out)",
        reference.len(),
        playback::meeting::speakers(&reference).len()
    );
    for (name, r) in &reports {
        let e = r.errors;
        println!(
            "{name}\n  WER {}  (sub {} del {} ins {})\n  deleted: {} where no line is, {} inside lines; {} said over another speaker",
            pct(r.wer),
            e.substitutions,
            e.deletions,
            e.insertions,
            r.deleted_uncovered,
            r.deleted_covered,
            r.deleted_overlapped
        );
        println!(
            "  of the deleted: {} stutter repeats, {} backchannels; most often: {}",
            r.deleted_repeats,
            r.deleted_backchannels,
            r.most_deleted
                .iter()
                .map(|(w, n)| format!("{w} {n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "  substituted most often: {}",
            r.most_substituted
                .iter()
                .map(|(w, n)| format!("{w} ({n})"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for h in &r.holes {
            println!("  hole {:.0}-{:.0} s: {} words", h.start, h.end, h.words);
        }
    }
    Ok(())
}

fn pct(rate: Option<f64>) -> String {
    rate.map_or_else(|| "-".into(), |r| format!("{:.1}%", r * 100.0))
}

fn score(dir: &Path, db: &Path, margin: i64, json: bool) -> Result<()> {
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
    let lines = lines(db, from, to)?;
    let report = score::score(&plan, &played, &lines, from, to);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let names: Vec<&str> = played.iter().map(|p| p.part.as_str()).collect();
    print!(
        "{:16} {:>7} {:>6} {:>6} {:>6}",
        "source", "WER", "sub", "del", "ins"
    );
    for n in &names {
        print!(" {n:>8}");
    }
    println!(" {:>9}", "invented");
    for inv in &report.invented {
        let t = score::total(&report, &inv.source);
        print!(
            "{:16} {:>7} {:>6} {:>6} {:>6}",
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

/// Who spoke when, from the plan as played: only turns with a known reader.
fn reference_turns(
    plan: &Plan,
    played: &[Played],
    mut detector: Option<&mut audiocore::vad::Detector>,
) -> Result<Vec<diarization::Turn>> {
    let mut out = Vec::new();
    for p in played {
        let Some(part) = plan.parts.iter().find(|x| x.name == p.part) else {
            continue;
        };
        for t in part
            .turns
            .iter()
            .filter(|t| t.speaker.starts_with("librispeech-"))
        {
            let start = p.start + score::seconds(t.offset);
            let spans = match detector.as_deref_mut() {
                Some(d) => {
                    let pcm =
                        audiocore::decode::decode_s16(Path::new(&t.audio), audiocore::vad::RATE)
                            .ok_or_else(|| format!("cannot decode {}", t.audio))?;
                    d.regions(&audiocore::decode::to_f32(&pcm))?
                        .into_iter()
                        .map(|r| (r.start, r.end))
                        .collect()
                }
                None => vec![(0.0, t.duration)],
            };
            for (s, e) in spans {
                out.push(diarization::Turn {
                    start: start + score::seconds(s),
                    end: start + score::seconds(e),
                    speaker: t.speaker.clone(),
                });
            }
        }
    }
    Ok(out)
}

fn diarize(dir: &Path, results: &Path, vad: bool) -> Result<()> {
    #[derive(serde::Deserialize)]
    struct Row {
        filename: String,
        source: String,
        result: String,
    }
    let plan = read_plan(dir)?;
    let mut detector = if vad {
        Some(audiocore::vad::Detector::load()?)
    } else {
        None
    };
    let reference = reference_turns(&plan, &read_played(dir)?, detector.as_mut())?;
    let rows: Vec<Row> = serde_json::from_slice(&std::fs::read(results)?)?;
    let mut clips = Vec::new();
    for row in rows {
        let start = audiocore::names::parse_segment_start(&row.filename)
            .ok_or_else(|| format!("no start in {}", row.filename))?;
        let stored: serde_json::Value = serde_json::from_str(&row.result)?;
        let mut turns = Vec::new();
        for t in stored["result"]["turns"].as_array().into_iter().flatten() {
            let (Some(s), Some(e), Some(who)) = (
                t["start"].as_f64(),
                t["end"].as_f64(),
                t["speaker"].as_str(),
            ) else {
                return Err(format!("{}: a turn without times or speaker", row.filename).into());
            };
            turns.push(diarization::Turn {
                start: start + score::seconds(s),
                end: start + score::seconds(e),
                speaker: who.to_owned(),
            });
        }
        clips.push(diarization::Clip {
            source: row.source,
            turns,
        });
    }
    println!(
        "{:12} {:>8} {:>10} {:>9} {:>7} {:>7} {:>11}",
        "source", "speech", "uncovered", "confused", "turns", "mixed", "miscounted"
    );
    for (source, s) in diarization::score(&reference, &clips) {
        let pct = |x: f64| format!("{:.1}%", 100.0 * x / s.reference_s.max(f64::EPSILON));
        println!(
            "{source:12} {:>7.0}s {:>10} {:>9} {:>7} {:>7} {:>5}/{:<5}",
            s.reference_s,
            pct(s.uncovered_s),
            pct(s.confused_s),
            s.turns,
            s.mixed_turns,
            s.clips_miscounted,
            s.clips
        );
    }
    println!(
        "uncovered, confused: share of played speech by a known reader; mixed: turns spanning two readers >= 1 s each"
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
        Command::Diarization { dir, results, vad } => diarize(&dir, &results, vad),
        Command::Meeting {
            words,
            meeting,
            hypothesis,
            json,
        } => meeting_score(&words, &meeting, &hypothesis, json),
        Command::Score {
            dir,
            db,
            margin,
            json,
        } => score(&dir, &db, margin, json),
    };
    if let Err(err) = done {
        eprintln!("playback: {err}");
        std::process::exit(1);
    }
}
