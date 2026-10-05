//! `runner`: the Mac's job orchestration.
//!
//! Lease the next job from recalld, fetch its audio, drive a model shim, push
//! the result. Stateless: killing it costs only a lease that expires.

use audiocore::job::Kind;
use audiocore::shim::{Stored, voices};
use chrono::Utc;
use clap::Parser;
use runner::client::{Client, Job, Span};
use runner::pulse::stamp_pulse;
use runner::shim::{self, Shim};
use std::path::Path;
use std::time::Duration;

const IDLE: Duration = Duration::from_secs(20);
const BACKOFF: Duration = Duration::from_mins(1);
/// How long a `--once` runner waits for its last beat before exiting.
const PULSE_SETTLE: Duration = Duration::from_secs(2);

/// What each shim can do, keyed by the name it reports in `hello` (not argv),
/// so a runner pointed at the wrong module leases nothing. An unknown name is
/// fatal.
fn kinds_for(shim_name: &str) -> Option<&'static [Kind]> {
    match shim_name {
        "asr" => Some(&[Kind::TranscribeSegment]),
        // `enroll-speaker` shares the process because both are pyannote; a
        // separate runner would load the weights twice.
        "voices" => Some(&[Kind::DiarizeSegment, Kind::EnrollSpeaker]),
        _ => None,
    }
}

struct Config {
    base: String,
    token: String,
    program: String,
    args: Vec<String>,
    once: bool,
    /// Where to stamp the archive's pulse (`<archive root>/worker-heartbeat.json`).
    /// Absent means do not stamp, for a runner that is not beside the archive.
    pulse: Option<std::path::PathBuf>,
}

/// Lease work from recalld and run it through a Python shim.
/// `RECALL_SYNC_TOKEN` must be set: the runner reads blobs and the queue, which
/// is the read plane, never a device token.
#[derive(Parser)]
#[command(name = "runner")]
struct Cli {
    /// The recall server.
    #[arg(long, default_value = "https://recall.xinutec.org")]
    url: String,
    /// One job, then exit.
    #[arg(long)]
    once: bool,
    /// Where to stamp the archive's pulse (`<archive root>/worker-heartbeat.json`).
    #[arg(long, value_name = "FILE")]
    pulse: Option<std::path::PathBuf>,
    /// The shim and its arguments: everything after it, verbatim [default:
    /// `python -m recall.shim_asr`].
    #[arg(long, num_args = 1.., allow_hyphen_values = true, value_name = "PROGRAM [ARGS]")]
    shim: Vec<String>,
}

fn parse_args() -> Config {
    let cli = Cli::parse();
    let Ok(token) = std::env::var("RECALL_SYNC_TOKEN") else {
        eprintln!("runner: RECALL_SYNC_TOKEN must be set");
        std::process::exit(2)
    };
    let (program, args) = match cli.shim.split_first() {
        Some((program, args)) => (program.clone(), args.to_vec()),
        None => (
            "python".to_owned(),
            vec!["-m".to_owned(), "recall.shim_asr".to_owned()],
        ),
    };
    Config {
        base: cli.url,
        token,
        program,
        args,
        once: cli.once,
        pulse: cli.pulse,
    }
}

/// Embed each named span of one clip. A refused span is skipped, so one corrupt
/// stretch does not cost the clip's other voices their enrolment.
fn embed_spans(
    shim: &mut Shim,
    clip: &Path,
    spans: &[Span],
) -> Result<(serde_json::Value, usize), shim::Error> {
    let mut prints = Vec::new();
    for span in spans {
        match shim.embed(clip, span.start_s, span.end_s) {
            Ok(answer) => {
                if let Some(vector) = answer.reply.vector {
                    prints.push(voices::Print {
                        segment_id: span.segment_id,
                        vector,
                    });
                }
            }
            Err(shim::Error::Refused(why)) => {
                tracing::warn!(segment_id = span.segment_id, %why, "span refused; skipping it");
            }
            // The shim itself is broken: end the job so the lease lapses and a
            // fresh process retries it.
            Err(other) => return Err(other),
        }
    }
    let rows = prints.len();
    let result = serde_json::to_value(voices::Prints { prints })
        .map_err(|e| shim::Error::Protocol(e.to_string()))?;
    Ok((result, rows))
}

/// A job's result as the fleet stores it (`audiocore::shim::Stored`).
fn stored(result: &Stored<serde_json::Value>) -> Result<String, serde_json::Error> {
    serde_json::to_string(result)
}

/// Do one job; `Ok(false)` when the queue was empty. An empty queue still
/// stamps the pulse (`rows == 0`), so the doctor can tell idle from gone.
fn one(
    client: &Client,
    shim: &mut Shim,
    kinds: &[Kind],
    scratch: &Path,
    prompt: Option<&str>,
    pulse: Option<&Path>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let started = Utc::now();
    let Some(job) = client.lease(kinds)? else {
        stamp_pulse(pulse, started, 0);
        return Ok(false);
    };
    let Job {
        id,
        kind,
        filename,
        source,
        spans,
        language,
    } = job;
    tracing::info!(id, %kind, %source, %filename, "leased");
    let clip = scratch.join(&filename);
    client.fetch_blob(&source, &filename, &clip)?;
    // Exhaustive: a new kind does not compile until it is given work here.
    let outcome = match kind {
        Kind::TranscribeSegment => shim
            .transcribe_in(&clip, language.as_deref(), prompt)
            .map(|a| (a.raw, a.reply.segments.len())),
        Kind::DiarizeSegment => shim.diarize(&clip).map(|a| (a.raw, a.reply.turns.len())),
        // One model call per named turn.
        Kind::EnrollSpeaker => embed_spans(shim, &clip, &spans),
    };
    // The scratch copy is removed whatever the outcome.
    let _ = std::fs::remove_file(&clip);
    match outcome {
        // Stored as the shim sent it; the rows come from the typed reply.
        Ok((result, rows)) => {
            client.finish(
                id,
                &stored(&Stored {
                    ok: true,
                    result: Some(result),
                    error: None,
                })?,
            )?;
            tracing::info!(id, rows, "done");
            stamp_pulse(pulse, started, rows);
            Ok(true)
        }
        // A refusal is recorded: the clip is the problem, and a retry would
        // get the same answer.
        Err(shim::Error::Refused(why)) => {
            tracing::warn!(id, %why, "shim refused; recording the failure");
            client.finish(
                id,
                &stored(&Stored {
                    ok: false,
                    result: None,
                    error: Some(why),
                })?,
            )?;
            // A refusal is a completed pass, not a stall.
            stamp_pulse(pulse, started, 0);
            Ok(true)
        }
        // Transport failure: let the lease expire so a fresh process retries.
        Err(err) => Err(Box::new(err)),
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let config = parse_args();
    let client = Client::new(&config.base, &config.token);
    let scratch = std::env::temp_dir().join("recall-runner");
    if let Err(err) = std::fs::create_dir_all(&scratch) {
        tracing::error!(%err, "cannot make a scratch directory");
        std::process::exit(1);
    }
    let mut shim = match Shim::spawn(&config.program, &config.args) {
        Ok(shim) => shim,
        Err(err) => {
            tracing::error!(%err, "cannot start the shim");
            std::process::exit(1);
        }
    };
    let name = match shim.hello() {
        Ok(name) => name,
        Err(err) => {
            tracing::error!(%err, "the shim did not answer hello");
            std::process::exit(1);
        }
    };
    let Some(kinds) = kinds_for(&name) else {
        tracing::error!(shim = %name, "unknown shim; refusing to guess what it can do");
        std::process::exit(1);
    };
    // Fatal if unreachable for a transcribing runner: an unbiased transcript
    // would have to be redone.
    let prompt = if kinds.contains(&Kind::TranscribeSegment) {
        match client.prompt() {
            Ok(prompt) => {
                tracing::info!(
                    terms = prompt.as_deref().map_or(0, |p| p.split(',').count()),
                    "vocabulary loaded"
                );
                prompt
            }
            Err(err) => {
                tracing::error!(%err, "cannot read the vocabulary; refusing to transcribe unbiased");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    tracing::info!(url = %config.base, shim = %name, kinds = ?kinds, "runner: polling");
    loop {
        match one(
            &client,
            &mut shim,
            kinds,
            &scratch,
            prompt.as_deref(),
            config.pulse.as_deref(),
        ) {
            // `--once` means one job, not until the queue empties.
            Ok(true) => {
                if config.once {
                    let _landed = runner::pulse::settle(PULSE_SETTLE);
                    return;
                }
            }
            Ok(false) => {
                if config.once {
                    let _landed = runner::pulse::settle(PULSE_SETTLE);
                    return;
                }
                std::thread::sleep(IDLE);
            }
            Err(err) => {
                tracing::warn!(%err, "job failed; backing off");
                if config.once {
                    std::process::exit(1);
                }
                std::thread::sleep(BACKOFF);
                // The shim may be dead: replace it.
                if let Ok(fresh) = Shim::spawn(&config.program, &config.args) {
                    shim = fresh;
                }
            }
        }
    }
}
