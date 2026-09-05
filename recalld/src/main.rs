//! recalld — see lib.rs and docs/architecture.md.
//!
//!   recalld --root <data-root> [--bind 127.0.0.1:8001] [--tokens <file>]
//!
//! `RECALLD_READ_TOKEN` (env, optional) gates the read side; per-source write
//! tokens come from `--tokens <file>` or the `RECALLD_INGEST_TOKENS` env var
//! (same line grammar). Everything unset = open, for dev and tests.

use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::tokens::Tokens;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

fn usage() -> ExitCode {
    eprintln!("usage: recalld --root <data-root> [--bind <addr:port>] [--tokens <file>]");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let mut root: Option<PathBuf> = None;
    let mut bind = String::from("127.0.0.1:8001");
    let mut tokens_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        let Some(value) = args.next() else {
            return usage();
        };
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--bind" => bind = value,
            "--tokens" => tokens_path = Some(PathBuf::from(value)),
            _ => return usage(),
        }
    }
    let Some(root) = root else {
        return usage();
    };
    // A configured-but-unreadable token table fails closed at startup: an
    // open ingest plane must be a choice, never the residue of a typo.
    let tokens = match (tokens_path, std::env::var("RECALLD_INGEST_TOKENS")) {
        (Some(path), _) => match Tokens::load(&path) {
            Ok(tokens) => Some(tokens),
            Err(err) => {
                eprintln!("recalld: cannot read tokens file {}: {err}", path.display());
                return ExitCode::FAILURE;
            }
        },
        (None, Ok(text)) if !text.trim().is_empty() => match Tokens::parse(&text) {
            Ok(tokens) => Some(tokens),
            Err(err) => {
                eprintln!("recalld: RECALLD_INGEST_TOKENS does not parse: {err}");
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    let read_token = std::env::var("RECALLD_READ_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    if let Err(err) = recalld::store::open(&root) {
        eprintln!(
            "recalld: cannot open {}/ingest.sqlite: {err}",
            root.display()
        );
        return ExitCode::FAILURE;
    }
    let config = Arc::new(Config {
        root,
        tokens,
        read_token,
        max_body_bytes: DEFAULT_MAX_BODY,
    });
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("recalld: runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(&bind).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("recalld: cannot bind {bind}: {err}");
                return ExitCode::FAILURE;
            }
        };
        spawn_level_scanner(config.root.clone());
        spawn_room_builder(config.root.clone());
        tracing::info!(%bind, "recalld: ingest plane listening");
        match axum::serve(listener, router(config)).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("recalld: serve: {err}");
                ExitCode::FAILURE
            }
        }
    })
}

/// Stage D2: the calibration scanner, measuring levels for every delivered
/// segment in bounded batches. Off the request path — its ffmpeg children and
/// sqlite writes ride `spawn_blocking`, and WAL keeps it from ever blocking
/// an upload.
fn spawn_level_scanner(root: PathBuf) {
    const BATCH: usize = 200;
    const IDLE: std::time::Duration = std::time::Duration::from_secs(30);
    const BACKOFF: std::time::Duration = std::time::Duration::from_mins(1);
    tokio::spawn(async move {
        loop {
            let batch_root = root.clone();
            let wrote =
                tokio::task::spawn_blocking(move || recalld::levels::scan_once(&batch_root, BATCH))
                    .await;
            match wrote {
                Ok(Ok(0)) => tokio::time::sleep(IDLE).await,
                Ok(Ok(n)) => tracing::info!(measured = n, "levels: batch complete"),
                Ok(Err(err)) => {
                    tracing::warn!(%err, "levels: scan failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
                Err(err) => {
                    tracing::error!(%err, "levels: task failed; backing off");
                    tokio::time::sleep(BACKOFF).await;
                }
            }
        }
    });
}

/// Stage D3: the room builder — one settled block at a time, calibrated
/// selection, terminal verdicts only. Chases the level scanner: a block whose
/// evidence is incomplete defers and returns next pass.
fn spawn_room_builder(root: PathBuf) {
    const IDLE: std::time::Duration = std::time::Duration::from_mins(1);
    tokio::spawn(async move {
        loop {
            let pass_root = root.clone();
            let config = recalld::room::RoomConfig::default();
            let built = tokio::task::spawn_blocking(move || {
                recalld::room::build_once(&pass_root, &config, chrono::Utc::now())
            })
            .await;
            match built {
                Ok(Ok(summary)) if summary.built + summary.silent > 0 => {
                    tracing::info!(
                        built = summary.built,
                        silent = summary.silent,
                        deferred = summary.deferred,
                        "room: pass complete"
                    );
                }
                Ok(Ok(_)) => tokio::time::sleep(IDLE).await,
                Ok(Err(err)) => {
                    tracing::warn!(%err, "room: pass failed; backing off");
                    tokio::time::sleep(IDLE).await;
                }
                Err(err) => {
                    tracing::error!(%err, "room: task failed; backing off");
                    tokio::time::sleep(IDLE).await;
                }
            }
        }
    });
}
