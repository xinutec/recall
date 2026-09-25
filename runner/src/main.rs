//! `runner`: the Mac's job orchestration.
//!
//! Poll recalld for the next job, fetch its audio, drive a model shim, push
//! the result, ack. Stateless, because the queue lives on Isis: killing it
//! costs only a lease that expires.
//!
//! `transcribe-segment` results become turns (`recalld::turns::PER_MIC`).
//! `transcribe-room` results are stored but not yet interpreted: the room
//! stream waits on the referee.

use audiocore::job::Kind;
use chrono::Utc;
use runner::client::{Client, Job, Span};
use runner::pulse::stamp_pulse;
use runner::shim::{self, Shim};
use std::path::Path;
use std::time::Duration;

const IDLE: Duration = Duration::from_secs(20);
const BACKOFF: Duration = Duration::from_mins(1);

/// What each shim can be given, keyed by the name it reports in `hello` rather
/// than inferred from argv, so a runner pointed at the wrong module does not
/// lease work it will fail. An unknown name is fatal.
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

fn usage() -> ! {
    eprintln!(
        "usage: runner --url <recalld> [--shim <program> [args...]] [--once]\n\
         \n\
         RECALL_SYNC_TOKEN must be set: the runner reads blobs and the queue,\n\
         which is the read plane, never a device token."
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut base = "http://10.100.0.2:8001".to_owned();
    let mut program = "python".to_owned();
    let mut args = vec!["-m".to_owned(), "recall.shim_asr".to_owned()];
    let mut once = false;
    let mut pulse: Option<std::path::PathBuf> = None;
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--url" => base = cli.next().unwrap_or_else(|| usage()),
            "--once" => once = true,
            "--pulse" => {
                pulse = Some(std::path::PathBuf::from(
                    cli.next().unwrap_or_else(|| usage()),
                ));
            }
            "--shim" => {
                program = cli.next().unwrap_or_else(|| usage());
                args = cli.by_ref().collect();
            }
            _ => usage(),
        }
    }
    let Ok(token) = std::env::var("RECALL_SYNC_TOKEN") else {
        usage()
    };
    Config {
        base,
        token,
        program,
        args,
        once,
        pulse,
    }
}

/// Embed each named span of one clip into the print list the fleet files.
///
/// A refused span is skipped, not fatal: otherwise one corrupt stretch would
/// cost every other voice in the clip its enrolment, and the fleet would record
/// the whole clip as decided.
fn embed_spans(
    shim: &mut Shim,
    clip: &Path,
    spans: &[Span],
) -> Result<serde_json::Value, shim::Error> {
    let mut prints = Vec::new();
    for span in spans {
        match shim.embed(clip, span.start_s, span.end_s) {
            Ok(answer) => {
                if let Some(vector) = answer.get("vector") {
                    prints.push(serde_json::json!({
                        "segment_id": span.segment_id,
                        "vector": vector,
                    }));
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
    Ok(serde_json::json!({ "prints": prints }))
}

/// Do one job. `Ok(false)` means the queue was empty.
///
/// An empty queue stamps the pulse with `rows == 0`, so the doctor can tell an
/// idle runner from a gone one. A runner wedged inside a job never stamps, so
/// the doctor still catches the stall.
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
    } = job;
    tracing::info!(id, %kind, %source, %filename, "leased");
    let clip = scratch.join(&filename);
    client.fetch_blob(&source, &filename, &clip)?;
    // Exhaustive: a new kind does not compile until it is given work here.
    let outcome = match kind {
        Kind::TranscribeSegment => shim.transcribe(&clip, None, prompt),
        Kind::DiarizeSegment => shim.diarize(&clip),
        // One model call per named turn, composed here. A refused span costs
        // only its print; the fleet re-derives it while its turn is unenrolled.
        Kind::EnrollSpeaker => embed_spans(shim, &clip, &spans),
    };
    // The scratch copy is removed whatever the outcome.
    let _ = std::fs::remove_file(&clip);
    match outcome {
        Ok(result) => {
            // Each kind names its result list differently: `segments` (asr),
            // `turns` (diarize), `prints` (enrol). A new kind needs its key here,
            // or its `rows` is always 0.
            let rows = ["segments", "turns", "prints"]
                .iter()
                .filter_map(|key| result.get(*key))
                .filter_map(serde_json::Value::as_array)
                .map(Vec::len)
                .sum::<usize>();
            client.finish(
                id,
                &serde_json::json!({ "ok": true, "result": result }).to_string(),
            )?;
            tracing::info!(id, rows, "done");
            stamp_pulse(pulse, started, rows);
            Ok(true)
        }
        // A refusal is terminal and recorded: the clip is the problem, not the
        // process, so a retry would get the same answer. The stored failure
        // shows which clips could not be processed.
        Err(shim::Error::Refused(why)) => {
            tracing::warn!(id, %why, "shim refused; recording the failure");
            client.finish(
                id,
                &serde_json::json!({ "ok": false, "error": why }).to_string(),
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
    // Fatal if unreachable, but only for a transcribing runner: unbiased
    // transcripts would have to be redone. An empty vocabulary is `None`, no
    // biasing. Diarization does not use it.
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
                    return;
                }
            }
            Ok(false) => {
                if config.once {
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
                // The shim may be dead; replace it rather than write to a
                // closed pipe.
                if let Ok(fresh) = Shim::spawn(&config.program, &config.args) {
                    shim = fresh;
                }
            }
        }
    }
}
