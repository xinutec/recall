//! `recall-live` — the instant feed, in the language the rest of the Mac is in.
//!
//! Read `audiod`'s live tap, cut it at the pauses, transcribe each utterance the
//! moment the speaker stops, push it to the fleet. Latency is the whole product:
//! a turn is on Isis's timeline two or three seconds after it is said, and the
//! archive pass replaces it with a properly derived one within the hour.
//!
//! ⚠ **It holds NO STATE — no store, no watermark, no scratch it must keep.**
//! The Python it replaces kept live turns in the Mac's `recall.sqlite` and
//! pushed them from a watermark on a second thread; both existed because the
//! Mac was once the system of record. It is not, so a live turn's only home is
//! Isis and the push IS the write. Kill this at any moment and nothing needs
//! recovering.

use audiocore::instant::python_isoformat_utc;
use chrono::Utc;
use runner::client::{Client, LiveTurn};
use runner::live::{self, Cutter, LIVE_MODEL, Tap, Utterance, spoken};
use runner::shim::{self, Shim};
use std::time::{Duration, Instant};

/// How long before the tap is reopened after it ends or fails to open. Short:
/// the tap ending is the ordinary consequence of capture restarting.
const REOPEN: Duration = Duration::from_secs(5);
/// How often the household glossary is re-read, so a name added in the UI
/// starts biasing the live feed without a restart.
const PROMPT_EVERY: Duration = Duration::from_mins(5);

struct Config {
    base: String,
    api: String,
    token: String,
    tap: String,
    program: String,
    args: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: recall-live [--url <recalld>] [--api <recall api>] [--tap <udp url>]\n\
        \x20                  [--shim <program> [args...]]\n\
         \n\
         RECALL_SYNC_TOKEN must be set: the instant feed writes to the meaning\n\
         plane, which is the sync plane's credential, never a device token."
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut base = "http://10.100.0.2:8001".to_owned();
    let mut api = "http://10.100.0.2:8000".to_owned();
    let mut tap = live::TAP.to_owned();
    let mut program = "python".to_owned();
    let mut args = vec!["-m".to_owned(), "recall.shim_asr".to_owned()];
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--url" => base = cli.next().unwrap_or_else(|| usage()),
            "--api" => api = cli.next().unwrap_or_else(|| usage()),
            "--tap" => tap = cli.next().unwrap_or_else(|| usage()),
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
        tap,
        program,
        args,
    }
}

/// Transcribe one utterance and push what it said.
///
/// ⚠ Errors are RETURNED, never propagated: the caller logs and takes the next
/// one. A live tier that stops on the first bad clip is a live tier that is off
/// while every health check stays green.
fn handle(
    shim: &mut Shim,
    client: &Client,
    prompt: Option<&str>,
    utterance: &Utterance,
) -> Result<(), Box<dyn std::error::Error>> {
    let clip = tempfile::Builder::new().suffix(".wav").tempfile()?;
    audiocore::wav::write_mono16(clip.path(), audiocore::vad::RATE, &utterance.samples)?;
    let result = shim.transcribe(clip.path(), None, prompt)?;
    let Some((text, language)) = spoken(&result) else {
        return Ok(());
    };
    let stored = client.push_live(&[LiveTurn {
        start: python_isoformat_utc(utterance.start),
        end: python_isoformat_utc(utterance.end),
        text,
        asr_model: LIVE_MODEL.to_owned(),
        language,
    }])?;
    tracing::info!(stored, "live turn pushed");
    Ok(())
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
    let (to_shim, utterances) = live::channel();

    // The transcriber owns the shim, the HTTP client and the glossary; the
    // reader below owns the tap and the detector and shares nothing with it.
    // Two threads, not the Python's three: its extra one existed to drain a
    // CoreAudio pipe that could overrun, and this reads a socket ffmpeg is
    // already allowed to drop from.
    let api = config.api.clone();
    let program = config.program.clone();
    let args = config.args.clone();
    let worker = std::thread::spawn(move || {
        let mut shim = match Shim::spawn(&program, &args) {
            Ok(shim) => shim,
            Err(err) => {
                tracing::error!(%err, "cannot start the shim; the instant feed is off");
                return;
            }
        };
        match shim.hello() {
            Ok(name) if name == "asr" => {}
            Ok(name) => {
                tracing::error!(shim = %name, "the instant feed needs the asr shim");
                return;
            }
            Err(err) => {
                tracing::error!(%err, "the shim did not answer hello");
                return;
            }
        }
        // ⚠ Best-effort, unlike the runner's, which is FATAL on the same call.
        // Transcribing the archive unbiased makes a corpus that has to be
        // redone; a live turn is superseded within the hour either way, so
        // refusing to speak until the glossary answers would cost more than it
        // saves.
        let mut prompt = client.prompt(&api).unwrap_or_else(|err| {
            tracing::warn!(%err, "no vocabulary; the live feed is unbiased for now");
            None
        });
        let mut refreshed = Instant::now();
        live::drain(&utterances, |utterance| {
            if refreshed.elapsed() >= PROMPT_EVERY {
                refreshed = Instant::now();
                if let Ok(fresh) = client.prompt(&api) {
                    prompt = fresh;
                }
            }
            if let Err(err) = handle(&mut shim, &client, prompt.as_deref(), &utterance) {
                tracing::warn!(%err, at = %utterance.start, "live utterance failed; continuing");
                // ⚠ Everything EXCEPT a refusal means the shim itself is
                // broken, and a broken shim cannot be written to — the loop
                // would spin against a closed pipe for ever, which is the
                // silent-forever failure this tier keeps finding. A refusal is
                // the clip's problem and the process is fine.
                if !matches!(
                    err.downcast_ref::<shim::Error>(),
                    Some(shim::Error::Refused(_))
                ) && let Ok(fresh) = Shim::spawn(&program, &args)
                {
                    tracing::warn!("restarted the shim");
                    shim = fresh;
                }
            }
        });
    });

    // ⚠ ONE cutter for the life of the process, not one per tap. `Detector::load`
    // takes a process-wide lock for `'static`, so a second live one would
    // deadlock — and re-loading the network on every capture restart would be
    // waste on top. Carrying it across a gap is safe because the tap ending
    // flushes whatever region was open.
    let mut cutter = match Cutter::open() {
        Ok(cutter) => cutter,
        Err(err) => {
            tracing::error!(%err, "cannot load the detector");
            std::process::exit(1);
        }
    };
    tracing::info!(url = %config.base, tap = %config.tap, "recall-live: reading the tap");
    loop {
        match Tap::open(&config.tap) {
            Ok(mut tap) => {
                let read = tap.windows(|window| match cutter.feed(window, Utc::now()) {
                    Ok(Some(utterance)) => live::offer(&to_shim, utterance),
                    Ok(None) => true,
                    Err(err) => {
                        tracing::error!(%err, "the detector failed");
                        false
                    }
                });
                if let Some(last) = cutter.flush(Utc::now()) {
                    live::offer(&to_shim, last);
                }
                // Zero windows means the tap idled out: capture is not running.
                // Ordinary, and it repeats every minute of a pause — so it must
                // not be the same log line as "capture restarted under us".
                if read == 0 {
                    tracing::debug!("the tap is idle; capture is not running");
                } else {
                    tracing::info!(windows = read, "the tap ended; reopening");
                }
            }
            Err(err) => tracing::warn!(%err, "cannot open the tap"),
        }
        // ⚠ EXIT rather than spin. A transcriber that has died takes the whole
        // point of the agent with it, and the loop above cannot tell — it would
        // read the tap for ever with every health check green. Exiting hands it
        // to `KeepAlive`, which is the one thing that can actually fix it.
        if worker.is_finished() {
            tracing::error!("the transcriber is gone; exiting so KeepAlive restarts us");
            std::process::exit(1);
        }
        std::thread::sleep(REOPEN);
    }
}
