//! The uploader against the REAL recalld — not the scripted stub in
//! tests/upload.rs, which proves the uploader's own logic but tests a copy of
//! the protocol. This is the pair that ships: audiod's `run_pass` delivering
//! to recalld's actual router, receipts computed by the actual store, tokens
//! checked by the actual gate.

use audiod::upload::{Config, run_pass};
use recalld::app::{Config as ServerConfig, router};
use recalld::tokens::Tokens;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// recalld on an ephemeral port, rooted in `server_root`, with the given
/// token table. Returns its base URL.
fn serve(server_root: &Path, tokens_text: &str) -> String {
    let tokens_path = server_root.join("tokens");
    std::fs::write(&tokens_path, tokens_text).expect("tokens");
    let config = Arc::new(ServerConfig {
        root: server_root.to_owned(),
        tokens: Some(Tokens::load(&tokens_path).expect("parse")),
        read_token: None,
        max_body_bytes: 16 * 1024 * 1024,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr")).expect("send");
            axum::serve(listener, router(config)).await.expect("serve");
        });
    });
    format!("http://{}", rx.recv().expect("addr"))
}

fn write_segment(root: &Path, source: &str, name: &str, bytes: &[u8]) {
    let dir = root.join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), bytes).expect("write");
}

#[test]
fn the_real_pair_delivers_verifies_and_stays_idempotent() {
    let archive = tempfile::tempdir().expect("archive");
    let server = tempfile::tempdir().expect("server");
    let url = serve(server.path(), "* custodial-token\n");
    write_segment(
        archive.path(),
        "usb",
        "usb-20260905T120000.opus",
        b"real bytes",
    );
    let config = Config {
        root: archive.path().to_owned(),
        base_url: url,
        token: Some("custodial-token".to_owned()),
        max_per_pass: 500,
        open_grace: Duration::ZERO,
    };
    let first = run_pass(&config);
    assert_eq!((first.uploaded, first.failed, first.conflicted), (1, 0, 0));
    // The blob recalld stored is byte-identical to the archive's copy.
    let stored = std::fs::read(server.path().join("ingest/usb/usb-20260905T120000.opus"))
        .expect("stored blob");
    assert_eq!(stored, b"real bytes");
    // A second pass has nothing to do — the state db remembers, and even if
    // it forgot, the server's PUT is idempotent.
    let second = run_pass(&config);
    assert_eq!((second.uploaded, second.failed), (0, 0));
}

#[test]
fn the_real_gate_refuses_a_wrong_token_and_nothing_is_recorded_delivered() {
    let archive = tempfile::tempdir().expect("archive");
    let server = tempfile::tempdir().expect("server");
    let url = serve(server.path(), "usb right-token\n");
    write_segment(archive.path(), "usb", "usb-20260905T120000.opus", b"bytes");
    let config = Config {
        root: archive.path().to_owned(),
        base_url: url,
        token: Some("wrong-token".to_owned()),
        max_per_pass: 500,
        open_grace: Duration::ZERO,
    };
    let pass = run_pass(&config);
    assert_eq!((pass.uploaded, pass.failed), (0, 1));
    assert!(
        !server
            .path()
            .join("ingest/usb/usb-20260905T120000.opus")
            .exists()
    );
}

#[test]
fn a_divergent_name_conflicts_through_the_real_409() {
    let archive = tempfile::tempdir().expect("archive");
    let server = tempfile::tempdir().expect("server");
    let url = serve(server.path(), "* tok\n");
    // The server already holds DIFFERENT bytes under the same name.
    std::fs::create_dir_all(server.path().join("ingest/usb")).expect("mkdir");
    write_segment(archive.path(), "usb", "usb-20260905T120000.opus", b"ours");
    // Deliver a first version from a second archive, then diverge.
    let other = tempfile::tempdir().expect("other");
    write_segment(other.path(), "usb", "usb-20260905T120000.opus", b"theirs");
    let seed = Config {
        root: other.path().to_owned(),
        base_url: url.clone(),
        token: Some("tok".to_owned()),
        max_per_pass: 500,
        open_grace: Duration::ZERO,
    };
    assert_eq!(run_pass(&seed).uploaded, 1);
    let config = Config {
        root: archive.path().to_owned(),
        ..seed
    };
    let pass = run_pass(&config);
    assert_eq!((pass.uploaded, pass.conflicted), (0, 1));
    // The first delivery's bytes stand untouched.
    let stored =
        std::fs::read(server.path().join("ingest/usb/usb-20260905T120000.opus")).expect("blob");
    assert_eq!(stored, b"theirs");
}
