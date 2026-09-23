//! recall-audiod: the audio-plane daemon (docs/architecture.md). One binary,
//! one subcommand per agent (see `usage`):
//!
//! * `ingest`: the network-mic ingest server.
//! * `capture`: the local-mic capture pipeline.
//! * `capture-mirror`: the Mac's pause mirror, which reports what it applied.
//! * `pause-mirror`: the pause mirror for a recorder that reports nothing.
//! * `upload`: one store-and-forward delivery pass (stage B).
//! * `beat-relay`: the LAN heartbeat fallback.
//! * `logrotate`: bound the agents' log files.
//! * `pause`, `resume`: the break-glass control.

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage: audiod ingest --root <data-root> [--port <port>]\n\
        \x20      audiod capture-mirror --root <data-root> --url <base> [--once]\n\
        \x20      audiod capture --root <data-root> --id <source> [--device <name>] [--seconds <n>] [--codec opus|flac]\n\
        \x20      audiod upload --root <data-root> --url <base> [--token-file <path>] [--max <n>]\n\
        \x20      audiod beat-relay --url <fleet> [--port <port>]\n\
        \x20      audiod pause-mirror --root <data-root> --url <base>\n\
        \x20      audiod logrotate\n\
        \n\
        \x20  the break-glass control, when the fleet cannot be reached:\n\
        \x20      audiod pause --root <data-root> [--minutes <n>]\n\
        \x20      audiod resume --root <data-root>"
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
    /// `pause` only: how long, or None for the full cap.
    minutes: Option<i64>,
    codec: audiod::segmenter::Codec,
}

/// Parse argv. `None` means the arguments do not name a run.
fn parse_args() -> Option<Args> {
    let mut args = std::env::args().skip(1);
    let mode = args.next();
    let mut root: Option<PathBuf> = None;
    // ⚠ Whether --port was given, not just its value: ingest and beat-relay
    // default to different ports, so one pre-seeded default would hand one of
    // them the other's.
    let mut port: Option<u16> = None;
    let mut id: Option<String> = None;
    let mut device: Option<String> = None;
    let mut seconds: Option<u64> = None;
    let mut url: Option<String> = None;
    let mut producer = audiod::capture_run::Producer::Sox;
    let mut token_file: Option<PathBuf> = None;
    let mut max: usize = 500;
    // None = the full MAX_PAUSE. `pause` is the only reader (audiod::pause).
    let mut minutes: Option<i64> = None;
    let mut codec = audiod::segmenter::CaptureConfig::default().codec;
    let mut once = false;
    while let Some(arg) = args.next() {
        // ⚠ Before the value fetch: every other flag takes a value, and a bare
        // `--once` would otherwise swallow the next argument.
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
            "--minutes" => match value.parse() {
                Ok(parsed) => minutes = Some(parsed),
                Err(_) => return None,
            },
            "--seconds" => match value.parse() {
                Ok(parsed) => seconds = Some(parsed),
                Err(_) => return None,
            },
            // Lossless is the prerequisite for combining microphones, not a
            // quality preference: Opus destroys phase, so two Opus streams of
            // one room cannot be summed coherently however well aligned.
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
        minutes,
        codec,
    })
}

/// Where launchd points the agents' stdio (`deploy/hm-agents.nix`).
fn logs_dir() -> PathBuf {
    std::env::var_os("RECALL_LOG_DIR").map_or_else(
        || {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join("Library/Logs/recall")
        },
        PathBuf::from,
    )
}

fn run_logrotate(dir: &Path) -> ExitCode {
    match audiod::logrotate::run(dir, audiod::logrotate::CAP_BYTES) {
        Ok(pass) => {
            tracing::info!(
                examined = pass.examined,
                rotated = pass.rotated,
                freed_mb = pass.freed_bytes / (1024 * 1024),
                "logrotate: pass complete"
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            tracing::error!(%err, dir = %dir.display(), "logrotate: pass failed");
            ExitCode::FAILURE
        }
    }
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
        minutes,
        codec,
    }) = parse_args()
    else {
        return usage();
    };

    // Above the root check on purpose — see `run_beat_relay`.
    if mode.as_deref() == Some("beat-relay") {
        return run_beat_relay(url.as_deref(), port);
    }
    // Also above it: the logs are not in the data root.
    if mode.as_deref() == Some("logrotate") {
        return run_logrotate(&logs_dir());
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
                url.as_deref(),
            )
        }
        Some("pause-mirror") => {
            let Some(url) = url else {
                return usage();
            };
            audiod::pause_mirror::run(&root, &url)
        }
        // The Mac's mirror: reports what it applied, then long-polls for
        // intent. The report is how the fleet knows a pause took hold.
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
        // The household's break-glass control. Here rather than in
        // `recall-cli`, which would need the network the emergency is about.
        Some("pause") => match audiod::pause::pause(&root, Utc::now(), minutes) {
            Ok(until) => {
                println!("paused until {}", until.to_rfc3339());
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("audiod pause: {err} — the pause did NOT take");
                ExitCode::FAILURE
            }
        },
        Some("resume") => match audiod::pause::resume(&root) {
            Ok(()) => {
                println!("resumed");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("audiod resume: {err}");
                ExitCode::FAILURE
            }
        },
        _ => usage(),
    }
}

/// The LAN heartbeat fallback: accept a beat, forward it to the fleet, forever.
/// Not gated on the pause, unlike `ingest`: a pause is exactly when the
/// heartbeat is the only signal there is.
///
/// Dispatched before the `--root` check: the relay stores nothing, so it has
/// no data root.
fn run_beat_relay(url: Option<&str>, port: Option<u16>) -> ExitCode {
    let Some(url) = url else {
        return usage();
    };
    let port = port.unwrap_or(audiod::beat_relay::DEFAULT_RELAY_PORT);
    let err = audiod::beat_relay::serve(port, url);
    eprintln!("audiod: beat-relay stopped: {err}");
    ExitCode::FAILURE
}

/// The Mac's capture mirror: report what was applied, long-poll for intent.
/// The token is the sync plane's, not the ingest one: `/sync/capture` is a
/// control-plane exchange.
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

/// The upload arm: resolve the token (file or env, never argv, which `ps`
/// shows) and run one bounded pass.
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
