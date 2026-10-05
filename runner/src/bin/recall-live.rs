//! `recall-live`: the instant feed (see `runner::live`). A turn reaches the
//! timeline two or three seconds after it is said; the archive pass replaces
//! it within the hour.

use audiocore::instant::python_isoformat_utc;
use chrono::Utc;
use clap::Parser;
use runner::client::{Client, LiveTurn};
use runner::live::{self, Cutter, LIVE_MODEL, Tap, Utterance, spoken};
use runner::shim::{self, Shim};
use std::time::{Duration, Instant};

/// Pause before reopening the tap. Short: the tap ends whenever capture
/// restarts.
const REOPEN: Duration = Duration::from_secs(5);
/// How often the vocabulary is re-read, so a name added in the UI applies
/// without a restart.
const PROMPT_EVERY: Duration = Duration::from_mins(5);

struct Config {
    base: String,
    token: String,
    tap: String,
    program: String,
    args: Vec<String>,
}

/// Transcribe audiod's live tap, one utterance at a time, and push each to
/// recall. Needs `RECALL_SYNC_TOKEN`; a device token cannot write live turns.
#[derive(Parser)]
#[command(name = "recall-live")]
struct Cli {
    /// The recall server.
    #[arg(long, default_value = "https://recall.xinutec.org")]
    url: String,
    /// The UDP tap to listen on.
    #[arg(long, value_name = "UDP_URL", default_value = live::TAP)]
    tap: String,
    /// The shim and its arguments: everything after it, verbatim [default:
    /// `python -m recall.shim_asr`].
    #[arg(long, num_args = 1.., allow_hyphen_values = true, value_name = "PROGRAM [ARGS]")]
    shim: Vec<String>,
}

fn parse_args() -> Config {
    let cli = Cli::parse();
    let Ok(token) = std::env::var("RECALL_SYNC_TOKEN") else {
        eprintln!("recall-live: RECALL_SYNC_TOKEN must be set");
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
        tap: cli.tap,
        program,
        args,
    }
}

/// Transcribe one utterance and push what it said.
fn handle(
    shim: &mut Shim,
    client: &Client,
    prompt: Option<&str>,
    utterance: &Utterance,
) -> Result<(), Box<dyn std::error::Error>> {
    let clip = tempfile::Builder::new().suffix(".wav").tempfile()?;
    audiocore::wav::write_mono16(clip.path(), audiocore::vad::RATE, &utterance.samples)?;
    let result = shim.transcribe(clip.path(), None, prompt)?.reply;
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

    // The transcriber thread owns the shim and the client; the reader below
    // owns the tap and the detector. They share only the channel.
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
        // Unlike the runner, not fatal: a live turn is replaced within the
        // hour anyway.
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
                // A refusal is the clip's fault; anything else may mean the
                // shim is dead.
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

    // One cutter for the process, not one per tap: the detector holds a
    // process-wide lock, so a second would deadlock. Each tap's end flushes
    // it.
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
                if read == 0 {
                    tracing::debug!("the tap is idle; capture is not running");
                } else {
                    tracing::info!(windows = read, "the tap ended; reopening");
                }
            }
            Err(err) => tracing::warn!(%err, "cannot open the tap"),
        }
        if worker.is_finished() {
            tracing::error!("the transcriber is gone; exiting so KeepAlive restarts us");
            std::process::exit(1);
        }
        std::thread::sleep(REOPEN);
    }
}
