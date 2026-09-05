//! The delivery half of the recorder contract, against a scripted receipt
//! server. What these prove: only closed segments ship, receipts are checked
//! by re-hash and not by status code, verified work is never resent, and a
//! conflict is journaled for a person instead of retried forever.
//!
//! The server here is a stub speaking recalld's receipt shape; the live pair
//! is proven by the stage-B shadow deployment, and a cross-crate test against
//! the real recalld arrives with stage D's workspace (docs/architecture.md).

use audiod::upload::{Config, run_pass};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One received PUT: (path, authorization header, body).
type Received = Arc<Mutex<Vec<(String, Option<String>, Vec<u8>)>>>;

enum Script {
    /// Answer every PUT with the true receipt for what arrived.
    Honest,
    /// Answer with a receipt for different bytes — a truncated store, a
    /// corrupt disk, a lying server; the uploader must not believe it.
    WrongHash,
    /// 409 every PUT — the name is held by different bytes.
    Conflict,
}

/// A minimal HTTP/1.1 server: enough to receive audiod's PUTs and answer per
/// the script. Runs until the listener drops.
fn serve(script: Script) -> (String, Received) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let received: Received = Arc::default();
    let log = Arc::clone(&received);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break None,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break Some(pos + 4);
                        }
                    }
                }
            };
            let Some(header_end) = header_end else {
                continue;
            };
            let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
            let path = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_owned();
            let auth = head.lines().find_map(|l| {
                l.strip_prefix("authorization: ")
                    .or_else(|| l.strip_prefix("Authorization: "))
                    .map(str::to_owned)
            });
            let length: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let mut body = buf[header_end..].to_vec();
            while body.len() < length {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => body.extend_from_slice(&chunk[..n]),
                }
            }
            log.lock().expect("lock").push((path, auth, body.clone()));
            let (status, payload) = match script {
                Script::Honest => (
                    "200 OK",
                    format!(
                        r#"{{"sha256":"{}","bytes":{}}}"#,
                        hex::encode(Sha256::digest(&body)),
                        body.len()
                    ),
                ),
                Script::WrongHash => (
                    "200 OK",
                    format!(
                        r#"{{"sha256":"{}","bytes":{}}}"#,
                        "0".repeat(64),
                        body.len()
                    ),
                ),
                Script::Conflict => (
                    "409 Conflict",
                    r#"{"error":"a different segment already holds this name"}"#.to_owned(),
                ),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{addr}"), received)
}

fn write_segment(root: &Path, source: &str, name: &str, bytes: &[u8]) {
    let dir = root.join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), bytes).expect("write");
}

fn config(root: &Path, url: &str, grace: Duration) -> Config {
    Config {
        root: root.to_owned(),
        base_url: url.to_owned(),
        token: Some("test-token".to_owned()),
        max_per_pass: 500,
        open_grace: grace,
    }
}

#[test]
fn closed_segments_ship_oldest_first_and_the_open_one_waits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (url, received) = serve(Script::Honest);
    write_segment(dir.path(), "usb", "usb-20260905T120000.opus", b"first");
    write_segment(dir.path(), "usb", "usb-20260905T120100.opus", b"second");
    // Freshly written and lexically newest: this is ffmpeg's open segment.
    write_segment(dir.path(), "usb", "usb-20260905T120200.opus", b"open");
    let summary = run_pass(&config(dir.path(), &url, audiod::upload::OPEN_GRACE));
    assert_eq!(summary.uploaded, 2);
    assert_eq!(summary.failed, 0);
    let log = received.lock().expect("lock");
    let paths: Vec<&str> = log.iter().map(|(p, _, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/ingest/v1/segments/usb/usb-20260905T120000.opus",
            "/ingest/v1/segments/usb/usb-20260905T120100.opus",
        ]
    );
    assert_eq!(log[0].1.as_deref(), Some("Bearer test-token"));
    assert_eq!(log[0].2, b"first");
}

#[test]
fn a_stale_newest_segment_is_closed_and_ships() {
    // Zero grace models "capture stopped long ago": the newest file's mtime
    // is already past the grace, so the ring's last segment is final.
    let dir = tempfile::tempdir().expect("tempdir");
    let (url, received) = serve(Script::Honest);
    write_segment(dir.path(), "usb", "usb-20260905T120000.opus", b"only");
    let summary = run_pass(&config(dir.path(), &url, Duration::ZERO));
    assert_eq!(summary.uploaded, 1);
    assert_eq!(received.lock().expect("lock").len(), 1);
}

#[test]
fn verified_work_is_never_resent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (url, received) = serve(Script::Honest);
    write_segment(dir.path(), "usb", "usb-20260905T120000.opus", b"bytes");
    let cfg = config(dir.path(), &url, Duration::ZERO);
    assert_eq!(run_pass(&cfg).uploaded, 1);
    let second = run_pass(&cfg);
    assert_eq!((second.uploaded, second.failed), (0, 0));
    assert_eq!(
        received.lock().expect("lock").len(),
        1,
        "one delivery total"
    );
}

#[test]
fn a_receipt_that_disagrees_with_our_own_hash_is_not_believed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (url, received) = serve(Script::WrongHash);
    write_segment(dir.path(), "usb", "usb-20260905T120000.opus", b"bytes");
    let cfg = config(dir.path(), &url, Duration::ZERO);
    let summary = run_pass(&cfg);
    assert_eq!((summary.uploaded, summary.failed), (0, 1));
    // Not recorded as delivered — the next pass tries again.
    run_pass(&cfg);
    assert_eq!(received.lock().expect("lock").len(), 2, "retried next pass");
}

#[test]
fn a_conflict_is_journaled_for_a_person_not_retried() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (url, received) = serve(Script::Conflict);
    write_segment(dir.path(), "usb", "usb-20260905T120000.opus", b"bytes");
    let cfg = config(dir.path(), &url, Duration::ZERO);
    let summary = run_pass(&cfg);
    assert_eq!((summary.uploaded, summary.conflicted), (0, 1));
    run_pass(&cfg);
    assert_eq!(
        received.lock().expect("lock").len(),
        1,
        "a 409 is terminal until a person clears it"
    );
}

#[test]
fn only_the_archive_naming_contract_ships() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (url, received) = serve(Script::Honest);
    write_segment(dir.path(), "usb", "usb-20260905T120000.opus", b"real");
    // Neighbours that must not ship: foreign names, wrong prefix, sidecars,
    // and non-source directories entirely.
    write_segment(dir.path(), "usb", "notes.txt", b"x");
    write_segment(dir.path(), "usb", "geb-20260905T120000.opus", b"x");
    write_segment(dir.path(), "clips", "meeting.ogg", b"x");
    std::fs::write(dir.path().join("ab-compare-x.md"), b"x").expect("write");
    let summary = run_pass(&config(dir.path(), &url, Duration::ZERO));
    assert_eq!(summary.uploaded, 1);
    let log = received.lock().expect("lock");
    assert_eq!(log.len(), 1);
    assert!(log[0].0.ends_with("/usb/usb-20260905T120000.opus"));
}
