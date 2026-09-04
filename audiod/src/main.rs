//! recall-audiod — the audio-plane daemon (docs/audio-plane.md).
//!
//!   audiod ingest  --root <archive> [--port 9999]
//!       the network-mic ingest server (the live recall-ingest agent)
//!   audiod capture --root <archive> --id usb [--device <CoreAudio name>]
//!       the local-mic capture pipeline (port of `recall record`; deployment
//!       still runs the Python capture agent until the flip)

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage: audiod ingest --root <data-root> [--port <port>]\n\
        \x20      audiod capture --root <data-root> --id <source> [--device <name>] [--seconds <n>]"
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
        _ => usage(),
    }
}
