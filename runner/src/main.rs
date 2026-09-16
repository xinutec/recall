//! `runner` — the Mac's whole job orchestration (stage E3).
//!
//! Poll recalld for the newest job, fetch its audio, drive a model shim, push
//! the result, ack. Stateless: no watermark, no outbox, no mirror queue, because
//! the queue lives on Isis. Kill it at any moment and the only cost is a lease
//! that expires.
//!
//! ⚠ **`transcribe-segment` is LIVE: its results become turns** — the per-mic
//! stream `recalld::turns::PER_MIC` writes, which replaced `worker.py` on
//! 2026-09-13. `transcribe-room` is not: its results are stored opaque and
//! nothing interprets them, because the room stream waits on the referee
//! (#1461), which cannot yet say which stream is better.

use chrono::Utc;
use runner::client::{Client, Job};
use runner::pulse::stamp_pulse;
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
        // ⚠ **THE CUTOVER.** `transcribe-segment` is here and
        // `org.xinutec.recall-worker` is gone, in the same commit, because they
        // are one change: both transcribe the same clips with the same model on
        // the same GPU, and running both would do all of it twice with the
        // second copy competing with capture.
        //
        // The lease orders by capture time across kinds (`queue::lease`), so a
        // room block and a microphone clip from the same minute compete on
        // equal terms rather than one starving the other.
        "asr" => Some(&["transcribe-room", "transcribe-segment"]),
        // ⚠ Both, and `diarize-segment` is the one that matters: it refines
        // clips that already carry turns, which is what `refine.py` does.
        // `diarize-room` refines the room stream, which is gated on #1461 — a
        // runner may hold both because the QUEUE decides which exist, and no
        // room job is derived while nothing writes room turns.
        "voices" => Some(&["diarize-segment", "diarize-room"]),
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
    /// Where to stamp the archive's pulse — `<archive root>/worker-heartbeat.json`.
    /// Absent means "do not stamp", which is right for a runner that is not
    /// beside the archive it would be certifying.
    pulse: Option<std::path::PathBuf>,
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
    let mut pulse: Option<std::path::PathBuf> = None;
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--url" => base = cli.next().unwrap_or_else(|| usage()),
            "--api" => api = cli.next().unwrap_or_else(|| usage()),
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
        api,
        token,
        program,
        args,
        once,
        pulse,
    }
}

/// Do one job. `Ok(false)` means the queue was empty.
///
/// ⚠ **An empty queue STAMPS the pulse.** The doctor cannot otherwise tell a
/// runner with nothing to do from one that is gone: both stamp nothing, and
/// "last pass 1024 min ago" reads as a stall when the truth is that the backlog
/// drained. It already renders `rows == 0` as "nothing to do" — it was simply
/// never sent such a beat.
///
/// A runner wedged INSIDE a job still never reaches here, so the stall it exists
/// to catch is still caught.
fn one(
    client: &Client,
    shim: &mut Shim,
    kinds: &[&str],
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
    } = job;
    tracing::info!(id, %kind, %source, %filename, "leased");
    let clip = scratch.join(&filename);
    client.fetch_blob(&source, &filename, &clip)?;
    // Only kinds this runner asked for can arrive; anything else is recalld
    // offering work the lease filter should have withheld, and saying so is
    // better than transcribing a diarization job by accident.
    let outcome = match kind.as_str() {
        // Both transcription kinds are the same work: one clip, one model, one
        // reply. What differs is which stream's turns it becomes, and that is
        // recalld's question, not the runner's.
        "transcribe-room" | "transcribe-segment" => shim.transcribe(&clip, None, prompt),
        "diarize-room" | "diarize-segment" => shim.diarize(&clip),
        other => Err(shim::Error::Refused(format!(
            "runner cannot do job kind {other}"
        ))),
    };
    // The scratch copy is the runner's only state, and it is gone either way.
    let _ = std::fs::remove_file(&clip);
    match outcome {
        Ok(result) => {
            // ⚠ **Each shim names its result differently, and counting only
            // one spelling makes the other's log line a constant.** `asr`
            // answers `segments`, `voices` answers `turns` — so a diarize job
            // logged `rows=0` whether it had found twelve speakers or none,
            // which is the one thing the line exists to say.
            let rows = ["segments", "turns"]
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
            // A refusal is a completed pass: the runner asked, the shim
            // answered, the queue moved. The doctor is watching for a STALLED
            // Mac, and a Mac refusing clips promptly is not that.
            stamp_pulse(pulse, started, 0);
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
        match client.prompt(&config.api) {
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
        match one(
            &client,
            &mut shim,
            kinds,
            &scratch,
            prompt.as_deref(),
            config.pulse.as_deref(),
        ) {
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
