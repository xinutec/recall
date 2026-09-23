//! `recall-live`: the instant feed.
//!
//! Read `audiod`'s live tap, cut it at the pauses, transcribe each utterance the
//! moment the speaker stops, push it to the fleet. A turn reaches Isis's
//! timeline two or three seconds after it is said, and the archive pass
//! replaces it within the hour.
//!
//! It holds no state: a live turn's only home is Isis, and the push is the
//! write. Kill it at any moment and nothing needs recovering.

use audiocore::instant::python_isoformat_utc;
use chrono::Utc;
use runner::client::{Client, LiveTurn};
use runner::live::{self, Cutter, LIVE_MODEL, Tap, Utterance, spoken};
use runner::shim::{self, Shim};
use std::time::{Duration, Instant};

/// How long before the tap is reopened after it ends or fails to open. Short:
/// the tap ending is the ordinary consequence of capture restarting.
const REOPEN: Duration = Duration::from_secs(5);
/// How often the vocabulary is re-read, so a name added in the UI starts
/// biasing the live feed without a restart.
const PROMPT_EVERY: Duration = Duration::from_mins(5);

struct Config {
    base: String,
    token: String,
    tap: String,
    program: String,
    args: Vec<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: recall-live [--url <recalld>] [--tap <udp url>]\n\
        \x20                  [--shim <program> [args...]]\n\
         \n\
         RECALL_SYNC_TOKEN must be set: the instant feed writes to the meaning\n\
         plane, which is the sync plane's credential, never a device token."
    );
    std::process::exit(2)
}

fn parse_args() -> Config {
    let mut base = "http://10.100.0.2:8001".to_owned();
    let mut tap = live::TAP.to_owned();
    let mut program = "python".to_owned();
    let mut args = vec!["-m".to_owned(), "recall.shim_asr".to_owned()];
    let mut cli = std::env::args().skip(1);
    while let Some(arg) = cli.next() {
        match arg.as_str() {
            "--url" => base = cli.next().unwrap_or_else(|| usage()),
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
        token,
        tap,
        program,
        args,
    }
}

/// Transcribe one utterance and push what it said.
///
/// Errors go back to the caller, which logs them and takes the next utterance,
/// so one bad clip does not stop the feed.
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

    // The transcriber thread owns the shim, the HTTP client and the
    // vocabulary; the reader below owns the tap and the detector. They share
    // only the channel.
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
        // Best-effort, unlike the runner, where it is fatal: a live turn is
        // superseded within the hour anyway.
        let mut prompt = client.prompt().unwrap_or_else(|err| {
            tracing::warn!(%err, "no vocabulary; the live feed is unbiased for now");
            None
        });
        let mut refreshed = Instant::now();
        live::drain(&utterances, |utterance| {
            if refreshed.elapsed() >= PROMPT_EVERY {
                refreshed = Instant::now();
                if let Ok(fresh) = client.prompt() {
                    prompt = fresh;
                }
            }
            if let Err(err) = handle(&mut shim, &client, prompt.as_deref(), &utterance) {
                tracing::warn!(%err, at = %utterance.start, "live utterance failed; continuing");
                // Anything but a refusal may mean the shim is broken, so
                // respawn it rather than write to a closed pipe for ever. A
                // refusal is the clip's problem.
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

    // ⚠ One cutter for the life of the process, not one per tap:
    // `Detector::load` holds a process-wide lock, so a second live one would
    // deadlock. Reusing it across taps is safe because each tap's end flushes
    // the open region.
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
                // Zero windows means the tap idled out because capture is not
                // running: ordinary, so logged at debug.
                if read == 0 {
                    tracing::debug!("the tap is idle; capture is not running");
                } else {
                    tracing::info!(windows = read, "the tap ended; reopening");
                }
            }
            Err(err) => tracing::warn!(%err, "cannot open the tap"),
        }
        // Exit if the transcriber has died, so `KeepAlive` restarts the agent
        // instead of the reader running on with nothing transcribed.
        if worker.is_finished() {
            tracing::error!("the transcriber is gone; exiting so KeepAlive restarts us");
            std::process::exit(1);
        }
        std::thread::sleep(REOPEN);
    }
}
