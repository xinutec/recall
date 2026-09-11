//! `runner` — the Mac's whole job orchestration (stage E3).
//!
//! Poll recalld for the newest job, fetch its audio, drive a model shim, push
//! the result, ack. Stateless: no watermark, no outbox, no mirror queue, because
//! the queue lives on Isis. Kill it at any moment and the only cost is a lease
//! that expires.
//!
//! ⚠ SHADOW BY CONSTRUCTION. Results are stored opaque by the queue and nothing
//! interprets them into turn rows yet, so running this changes no transcript
//! anyone reads. The flip — retiring the old worker — waits on the referee
//! (#1461), which cannot yet say which room stream is better.

use runner::client::{self, Client, Job};
use runner::shim::{self, Shim};
use std::path::Path;
use std::time::Duration;

const IDLE: Duration = Duration::from_secs(20);
const BACKOFF: Duration = Duration::from_mins(1);

/// What each shim can be given. The shim NAMES ITSELF over the protocol
/// (`hello`), so this is discovered at startup rather than inferred from argv —
/// a runner pointed at the wrong module would otherwise lease work confidently
/// and fail every job of it.
///
/// An unknown name is FATAL. Guessing "it is probably asr" is how a `voices`
/// process ends up holding transcription jobs it can only refuse.
fn kinds_for(shim_name: &str) -> Option<&'static [&'static str]> {
    match shim_name {
        "asr" => Some(&["transcribe-room"]),
        "voices" => Some(&["diarize-room"]),
        _ => None,
    }
}

struct Config {
    base: String,
    api: String,
    token: String,
    program: String,
    args: Vec<String>,
    once: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: runner --url <recalld> [--api <recall api>] [--shim <program> [args...]] [--once]\n\
         \n\
         RECALL_SYNC_TOKEN must be set: the runner reads blobs and the queue,\n\
         which is the read plane, never a device token."
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut base = "http://10.100.0.2:8001".to_owned();
    let mut api = "http://10.100.0.2:8000".to_owned();
    let mut program = "python".to_owned();
    let mut args = vec!["-m".to_owned(), "recall.shim_asr".to_owned()];
    let mut once = false;
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--url" => base = cli.next().unwrap_or_else(|| usage()),
            "--api" => api = cli.next().unwrap_or_else(|| usage()),
            "--once" => once = true,
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
        api,
        token,
        program,
        args,
        once,
    }
}

/// Do one job. `Ok(false)` means the queue was empty.
fn one(
    client: &Client,
    shim: &mut Shim,
    kinds: &[&str],
    scratch: &Path,
    prompt: Option<&str>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let Some(job) = client.lease(kinds)? else {
        return Ok(false);
    };
    let Job { id, kind, filename } = job;
    tracing::info!(id, %kind, %filename, "leased");
    let clip = scratch.join(&filename);
    client.fetch_blob("room", &filename, &clip)?;
    // Only kinds this runner asked for can arrive; anything else is recalld
    // offering work the lease filter should have withheld, and saying so is
    // better than transcribing a diarization job by accident.
    let outcome = match kind.as_str() {
        "transcribe-room" => shim.transcribe(&clip, None, prompt),
        "diarize-room" => shim.diarize(&clip),
        other => Err(shim::Error::Refused(format!(
            "runner cannot do job kind {other}"
        ))),
    };
    // The scratch copy is the runner's only state, and it is gone either way.
    let _ = std::fs::remove_file(&clip);
    match outcome {
        Ok(result) => {
            client.finish(
                id,
                &serde_json::json!({ "ok": true, "result": result }).to_string(),
            )?;
            tracing::info!(id, "done");
            Ok(true)
        }
        // ⚠ A REFUSAL IS TERMINAL, and recorded. The shim answered — the clip is
        // the problem, not the process — so retrying it would burn the same
        // answer for ever. Storing the failure means a later reader can see
        // WHICH clips could not be transcribed, instead of finding a silent gap.
        Err(shim::Error::Refused(why)) => {
            tracing::warn!(id, %why, "shim refused; recording the failure");
            client.finish(
                id,
                &serde_json::json!({ "ok": false, "error": why }).to_string(),
            )?;
            Ok(true)
        }
        // Transport failures are the SHIM's problem: say nothing, let the lease
        // expire, and let a fresh process try the same job.
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
    // ⚠ FATAL if unreachable, deliberately — but only for a runner that will
    // TRANSCRIBE. Transcribing without the biasing the vocabulary was built for
    // produces a corpus that has to be redone, and re-transcription is the cost
    // #1388 exists to reduce. An EMPTY vocabulary is fine — that is `None`, and
    // means no biasing rather than a failure. Diarization has no use for it, and
    // making a `voices` runner die on an unreachable fleet would be a dependency
    // it does not have.
    let prompt = if kinds.contains(&"transcribe-room") {
        match client::fetch_prompt(&config.api, &config.token) {
            Ok(prompt) => {
                tracing::info!(
                    terms = prompt.as_deref().map_or(0, |p| p.split(',').count()),
                    "vocabulary loaded"
                );
                prompt
            }
            Err(err) => {
                tracing::error!(%err, api = %config.api, "cannot read the vocabulary; refusing to transcribe unbiased");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    tracing::info!(url = %config.base, shim = %name, kinds = ?kinds, "runner: polling");
    loop {
        match one(&client, &mut shim, kinds, &scratch, prompt.as_deref()) {
            // ⚠ `--once` means ONE JOB, not "until the queue empties". It read
            // the latter on 2026-09-06 and chewed through six live jobs during
            // what was meant to be a single end-to-end check.
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
                // A dead shim cannot be written to; get a fresh one rather than
                // spin against a closed pipe.
                if let Ok(fresh) = Shim::spawn(&config.program, &config.args) {
                    shim = fresh;
                }
            }
        }
    }
}
