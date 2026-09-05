//! recall-audiod — the audio-plane daemon (docs/audio-plane.md).
//!
//!   audiod ingest  --root <archive> [--port 9999]
//!       the network-mic ingest server (the live recall-ingest agent)
//!   audiod capture --root <archive> --id usb [--device <CoreAudio name>]
//!       the local-mic capture pipeline (port of `recall record`; deployment
//!       still runs the Python capture agent until the flip)
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
        \x20      audiod capture --root <data-root> --id <source> [--device <name>] [--seconds <n>]\n\
        \x20      audiod upload --root <data-root> --url <base> [--token-file <path>] [--max <n>]"
    );
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
    let mode = args.next();
    let mut root: Option<PathBuf> = None;
    let mut port: u16 = audiod::wire::DEFAULT_INGEST_PORT;
    let mut id: Option<String> = None;
    let mut device: Option<String> = None;
    let mut seconds: Option<u64> = None;
    let mut url: Option<String> = None;
    let mut token_file: Option<PathBuf> = None;
    let mut max: usize = 500;
    while let Some(arg) = args.next() {
        let Some(value) = args.next() else {
            return usage();
        };
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--port" => match value.parse() {
                Ok(parsed) => port = parsed,
                Err(_) => return usage(),
            },
            "--id" => id = Some(value),
            "--device" => device = Some(value),
            "--url" => url = Some(value),
            "--token-file" => token_file = Some(PathBuf::from(value)),
            "--max" => match value.parse() {
                Ok(parsed) => max = parsed,
                Err(_) => return usage(),
            },
            "--seconds" => match value.parse() {
                Ok(parsed) => seconds = Some(parsed),
                Err(_) => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(root) = root else {
        return usage();
    };
    let config = audiod::segmenter::CaptureConfig::default();
    match mode.as_deref() {
        Some("ingest") => audiod::server::serve(&root, port, &config),
        Some("capture") => {
            let Some(id) = id else {
                return usage();
            };
            audiod::capture_run::serve_paused_aware(&root, &id, device.as_deref(), &config, seconds)
        }
        Some("upload") => {
            let Some(url) = url else {
                return usage();
            };
            // The token rides a file or the environment, never argv: argv is
            // world-readable in `ps`, and the fleet's secrets stay out of the
            // nix store the same way (.env, sourced by the agent wrapper).
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
        _ => usage(),
    }
}
