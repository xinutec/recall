//! recall-audiod — the audio-plane daemon (docs/audio-plane.md).
//!
//!   audiod ingest  --root <archive> [--port 9999]
//!       the network-mic ingest server (the live recall-ingest agent)
//!   audiod capture --root <archive> --id usb [--device <CoreAudio name>]
//!       the local-mic capture pipeline (port of `recall record`; deployment
//!       still runs the Python capture agent until the flip)
//!   audiod pause-mirror --root <archive> --url <control base>
//!       maintain `capture_paused_until` from the control plane's word — the
//!       recorder-contract pause for hosts with no local mirror (geb)
//!   audiod upload  --root <archive> --url <recalld base> [--token-file <path>]
//!       one store-and-forward delivery pass (docs/architecture.md, stage B):
//!       closed segments → recalld, sha-256 receipts verified, state recorded.
//!       The token comes from `--token-file` or the `RECALL_INGEST_TOKEN` env var
//!       (the launchd agent sources it from .env — never the nix store)

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage: audiod ingest --root <data-root> [--port <port>]\n\
        \x20      audiod capture-mirror --root <data-root> --url <base> [--once]\n\x20      audiod capture --root <data-root> --id <source> [--device <name>] [--seconds <n>] [--codec opus|flac]\n\
        \x20      audiod upload --root <data-root> --url <base> [--token-file <path>] [--max <n>]\n\
        \x20      audiod beat-relay --url <fleet> [--port <port>]"
    );
    ExitCode::FAILURE
}

/// Everything the command line can say, parsed once.
struct Args {
    mode: Option<String>,
    once: bool,
    root: Option<PathBuf>,
    port: Option<u16>,
    id: Option<String>,
    device: Option<String>,
    seconds: Option<u64>,
    url: Option<String>,
    producer: audiod::capture_run::Producer,
    token_file: Option<PathBuf>,
    max: usize,
    codec: audiod::segmenter::Codec,
}

/// Parse argv. `None` means the arguments do not name a run.
fn parse_args() -> Option<Args> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    let mut root: Option<PathBuf> = None;
    // ⚠ Whether --port was GIVEN, not just its value. Two subcommands listen and
    // their defaults differ (ingest 9999, beat-relay 8000), so a single
    // pre-seeded default silently hands one of them the other's port.
    let mut port: Option<u16> = None;
    let mut id: Option<String> = None;
    let mut device: Option<String> = None;
    let mut seconds: Option<u64> = None;
    let mut url: Option<String> = None;
    let mut producer = audiod::capture_run::Producer::Sox;
    let mut token_file: Option<PathBuf> = None;
    let mut max: usize = 500;
    let mut codec = audiod::segmenter::CaptureConfig::default().codec;
    let mut once = false;
    while let Some(arg) = args.next() {
        // ⚠ Handled BEFORE the value fetch: every other flag takes one, and a
        // bare `--once` would otherwise swallow the next argument.
        if arg == "--once" {
            once = true;
            continue;
        }
        // A flag with no value is not a run; `main` turns None into usage.
        let value = args.next()?;
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--port" => match value.parse() {
                Ok(parsed) => port = Some(parsed),
                Err(_) => return None,
            },
            "--id" => id = Some(value),
            "--device" => device = Some(value),
            "--url" => url = Some(value),
            "--producer" => match value.as_str() {
                "sox" => producer = audiod::capture_run::Producer::Sox,
                "alsa" => producer = audiod::capture_run::Producer::Alsa,
                _ => return None,
            },
            "--token-file" => token_file = Some(PathBuf::from(value)),
            "--max" => match value.parse() {
                Ok(parsed) => max = parsed,
                Err(_) => return None,
            },
            "--seconds" => match value.parse() {
                Ok(parsed) => seconds = Some(parsed),
                Err(_) => return None,
            },
            // ⚠ Lossless is the prerequisite for COMBINING microphones, not a
            // quality preference. Opus at 32 kbps is transparent to an ear and
            // destructive to phase — it codes what you notice rather than the
            // waveform — so two Opus streams of one room cannot be summed
            // coherently however well they are aligned.
            "--codec" => match value.as_str() {
                "opus" => codec = audiod::segmenter::Codec::Libopus,
                "flac" => codec = audiod::segmenter::Codec::Flac,
                _ => return None,
            },
            _ => return None,
        }
    }
    Some(Args {
        mode,
        once,
        root,
        port,
        id,
        device,
        seconds,
        url,
        producer,
        token_file,
        max,
        codec,
    })
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let Some(Args {
        mode,
        once,
        root,
        port,
        id,
        device,
        seconds,
        url,
        producer,
        token_file,
        max,
        codec,
    }) = parse_args()
    else {
        return usage();
    };

    // Above the root check on purpose — see `run_beat_relay`.
    if mode.as_deref() == Some("beat-relay") {
        return run_beat_relay(url.as_deref(), port);
    }
    let Some(root) = root else {
        return usage();
    };
    let config = audiod::segmenter::CaptureConfig {
        codec,
        bitrate: codec.default_bitrate().map(Into::into),
        ..audiod::segmenter::CaptureConfig::default()
    };
    match mode.as_deref() {
        Some("ingest") => audiod::server::serve(
            &root,
            port.unwrap_or(audiod::wire::DEFAULT_INGEST_PORT),
            &config,
        ),
        Some("capture") => {
            let Some(id) = id else {
                return usage();
            };
            audiod::capture_run::serve_paused_aware(
                &root,
                &id,
                device.as_deref(),
                producer,
                &config,
                seconds,
            )
        }
        Some("pause-mirror") => {
            let Some(url) = url else {
                return usage();
            };
            audiod::pause_mirror::run(&root, &url)
        }
        // The MAC's mirror: reports what it applied, then long-polls for intent.
        // Distinct from `pause-mirror` (geb) because reporting is the difference
        // — the fleet has no other way to know a pause took hold.
        Some("capture-mirror") => match url {
            None => usage(),
            Some(url) => run_capture_mirror(&root, &url, token_file, once),
        },
        Some("upload") => {
            let Some(url) = url else {
                return usage();
            };
            run_upload(root, url, token_file, max)
        }
        Some("speech") => run_speech(&root, max),
        _ => usage(),
    }
}

/// The speech arm: one bounded pass of the archive's unmeasured segments.
///
/// ⚠ Bounded on purpose, and low priority in the agent that drives it. This
/// decodes audio, and the machine it runs on is also recording: delivery must
/// never compete with the recorder (design.md §7). A 13k-segment backlog is
/// meant to drain over days behind live capture, not in one greedy pass.
fn run_speech(root: &std::path::Path, max: usize) -> ExitCode {
    match audiod::speech_scan::run(root, max) {
        Ok(pass) => {
            let left = audiod::speech_scan::remaining(root).unwrap_or(-1);
            tracing::info!(
                measured = pass.measured,
                unreadable = pass.unreadable,
                remaining = left,
                "speech: pass complete"
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("audiod speech: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The upload arm: resolve the token (file or env — never argv, which is
/// world-readable in `ps`; the fleet's secrets stay out of the nix store the
/// same way) and run one bounded pass.
/// The Mac's capture mirror: report what was applied, long-poll for intent.
///
/// The token is the SYNC plane's, not the ingest one: `/sync/capture` is a
/// control-plane exchange, and the mirror presents the same credential
/// `recall.sync` did.
/// The LAN heartbeat fallback: accept a beat, forward it to the fleet, forever.
///
/// ⚠ NOT gated on the pause, unlike `ingest` — a pause is exactly when the
/// heartbeat is the only signal there is.
///
/// ⚠ **Dispatched BEFORE the `--root` check, and that is the point rather than
/// an ordering accident.** The relay forwards and stores nothing, so it has no
/// data root. Requiring one would say it keeps a local beat store — the very
/// thing `beat_relay` refuses, because two places disagreeing about which mics
/// are alive is worse than the bug it fixes.
fn run_beat_relay(url: Option<&str>, port: Option<u16>) -> ExitCode {
    let Some(url) = url else {
        return usage();
    };
    let port = port.unwrap_or(audiod::beat_relay::DEFAULT_RELAY_PORT);
    let err = audiod::beat_relay::serve(port, url);
    eprintln!("audiod: beat-relay stopped: {err}");
    ExitCode::FAILURE
}

fn run_capture_mirror(
    root: &std::path::Path,
    url: &str,
    token_file: Option<PathBuf>,
    once: bool,
) -> ExitCode {
    let token = match token_file {
        None => std::env::var("RECALL_SYNC_TOKEN").unwrap_or_default(),
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => text.trim().to_owned(),
            Err(err) => {
                eprintln!("audiod: cannot read token file {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        },
    };
    if token.is_empty() {
        eprintln!("audiod: capture-mirror needs RECALL_SYNC_TOKEN or --token-file");
        return ExitCode::FAILURE;
    }
    let interval = std::time::Duration::from_secs(5);
    if once {
        return audiod::pause_mirror::exchange_once(root, url, &token, interval);
    }
    audiod::pause_mirror::run_exchange(root, url, &token, interval)
}

fn run_upload(root: PathBuf, url: String, token_file: Option<PathBuf>, max: usize) -> ExitCode {
    let token = match token_file {
        None => std::env::var("RECALL_INGEST_TOKEN")
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty()),
        Some(path) => match std::fs::read_to_string(&path) {
            Ok(text) => Some(text.trim().to_owned()),
            Err(err) => {
                eprintln!("audiod: cannot read token file {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        },
    };
    let summary = audiod::upload::run_pass(&audiod::upload::Config {
        root,
        base_url: url,
        token,
        max_per_pass: max,
        open_grace: audiod::upload::OPEN_GRACE,
    });
    if summary.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
