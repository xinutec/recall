//! recall-audiod: the audio-plane daemon (docs/architecture.md). One binary,
//! one subcommand per agent (see `--help`):
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
use clap::{Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The audio-plane daemon: one subcommand per agent.
#[derive(Parser)]
#[command(name = "audiod")]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// The network-mic ingest server.
    Ingest {
        #[command(flatten)]
        root: Root,
        /// [default: the ingest port]
        #[arg(long)]
        port: Option<u16>,
        #[command(flatten)]
        codec: CodecArg,
    },
    /// The local-mic capture pipeline.
    Capture {
        #[command(flatten)]
        root: Root,
        /// The source's name.
        #[arg(long)]
        id: String,
        /// The input device [default: the system's].
        #[arg(long)]
        device: Option<String>,
        /// Segment length, in seconds [default: the segmenter's].
        #[arg(long)]
        seconds: Option<u64>,
        #[arg(long, value_enum, default_value_t = ProducerArg::Sox)]
        producer: ProducerArg,
        #[command(flatten)]
        codec: CodecArg,
        /// The fleet's control plane, for the pause it reports.
        #[arg(long)]
        url: Option<String>,
    },
    /// The Mac's pause mirror, which reports what it applied.
    CaptureMirror {
        #[command(flatten)]
        root: Root,
        #[arg(long)]
        url: String,
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// One exchange, then exit.
        #[arg(long)]
        once: bool,
    },
    /// The pause mirror for a recorder that reports nothing.
    PauseMirror {
        #[command(flatten)]
        root: Root,
        #[arg(long)]
        url: String,
    },
    /// One store-and-forward delivery pass (stage B).
    Upload {
        #[command(flatten)]
        root: Root,
        #[arg(long)]
        url: String,
        /// [default: `RECALL_INGEST_TOKEN`]
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// Segments per pass.
        #[arg(long, default_value_t = 500)]
        max: usize,
    },
    /// The LAN heartbeat fallback. Stores nothing, so it takes no root.
    BeatRelay {
        /// The fleet.
        #[arg(long)]
        url: String,
        /// [default: the relay port]
        #[arg(long)]
        port: Option<u16>,
    },
    /// Bound the agents' log files.
    Logrotate,
    /// Break-glass, when the fleet cannot be reached: stop recording.
    Pause {
        #[command(flatten)]
        root: Root,
        /// How long [default: the full cap].
        #[arg(long)]
        minutes: Option<i64>,
    },
    /// Break-glass: record again.
    Resume {
        #[command(flatten)]
        root: Root,
    },
}

#[derive(clap::Args)]
struct Root {
    /// The data root.
    #[arg(long = "root", value_name = "DATA_ROOT")]
    path: PathBuf,
}

#[derive(clap::Args)]
struct CodecArg {
    /// Lossless is the prerequisite for combining microphones, not a quality
    /// preference: Opus destroys phase, so two Opus streams of one room cannot
    /// be summed coherently however well aligned.
    #[arg(long = "codec", value_enum, default_value_t = CodecName::Flac)]
    name: CodecName,
}

#[derive(Clone, Copy, ValueEnum)]
enum CodecName {
    Opus,
    Flac,
}

impl CodecArg {
    fn config(&self) -> audiod::segmenter::CaptureConfig {
        let codec = match self.name {
            CodecName::Opus => audiod::segmenter::Codec::Libopus,
            CodecName::Flac => audiod::segmenter::Codec::Flac,
        };
        audiod::segmenter::CaptureConfig {
            codec,
            bitrate: codec.default_bitrate().map(Into::into),
            ..audiod::segmenter::CaptureConfig::default()
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ProducerArg {
    Sox,
    Alsa,
}

impl From<ProducerArg> for audiod::capture_run::Producer {
    fn from(p: ProducerArg) -> Self {
        match p {
            ProducerArg::Sox => Self::Sox,
            ProducerArg::Alsa => Self::Alsa,
        }
    }
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
    match Cli::parse().mode {
        Mode::BeatRelay { url, port } => run_beat_relay(&url, port),
        Mode::Logrotate => run_logrotate(&logs_dir()),
        Mode::Ingest { root, port, codec } => audiod::server::serve(
            &root.path,
            port.unwrap_or(audiod::wire::DEFAULT_INGEST_PORT),
            &codec.config(),
        ),
        Mode::Capture {
            root,
            id,
            device,
            seconds,
            producer,
            codec,
            url,
        } => audiod::capture_run::serve_paused_aware(
            &root.path,
            &id,
            device.as_deref(),
            producer.into(),
            &codec.config(),
            seconds,
            url.as_deref(),
        ),
        Mode::PauseMirror { root, url } => audiod::pause_mirror::run(&root.path, &url),
        // The Mac's mirror: reports what it applied, then long-polls for
        // intent. The report is how the fleet knows a pause took hold.
        Mode::CaptureMirror {
            root,
            url,
            token_file,
            once,
        } => run_capture_mirror(&root.path, &url, token_file, once),
        Mode::Upload {
            root,
            url,
            token_file,
            max,
        } => run_upload(root.path, url, token_file, max),
        // The household's break-glass control. Here rather than in
        // `recall-cli`, which would need the network the emergency is about.
        Mode::Pause { root, minutes } => {
            match audiod::pause::pause(&root.path, Utc::now(), minutes) {
                Ok(until) => {
                    println!("paused until {}", until.to_rfc3339());
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("audiod pause: {err} — the pause did NOT take");
                    ExitCode::FAILURE
                }
            }
        }
        Mode::Resume { root } => match audiod::pause::resume(&root.path) {
            Ok(()) => {
                println!("resumed");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("audiod resume: {err}");
                ExitCode::FAILURE
            }
        },
    }
}

/// The LAN heartbeat fallback: accept a beat, forward it to the fleet, forever.
/// Not gated on the pause, unlike `ingest`: a pause is exactly when the
/// heartbeat is the only signal there is.
///
/// It takes no `--root`: the relay stores nothing.
fn run_beat_relay(url: &str, port: Option<u16>) -> ExitCode {
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
