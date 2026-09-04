//! recall-audiod — the audio-plane daemon. Today: the network-mic ingest
//! server (the port of `recall ingest-server`). The USB capture and the
//! fusion engine land here next; see docs/audio-plane.md.

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!("usage: audiod --root <data-root> [--port <port>]");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let mut root: Option<PathBuf> = None;
    let mut port: u16 = audiod::wire::DEFAULT_INGEST_PORT;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => match args.next() {
                Some(value) => root = Some(PathBuf::from(value)),
                None => return usage(),
            },
            "--port" => match args.next().and_then(|v| v.parse().ok()) {
                Some(value) => port = value,
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let Some(root) = root else {
        return usage();
    };
    let config = audiod::segmenter::CaptureConfig::default();
    audiod::server::serve(&root, port, &config)
}
