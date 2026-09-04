//! One device connection end to end, against a stub segmenter: handshake in,
//! PCM pumped through, liveness marked, both capture events recorded. The
//! accept/pause loop is deliberately not driven here — it is thin, and its
//! parts (pause file, handshake bounds) have their own tests.

mod common;

use audiod::segmenter::CaptureConfig;
use audiod::server::handle_connection;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// A segmenter stand-in: ignores the ffmpeg argv, writes its stdin to a
/// realistically named segment file under the source directory (the last argv
/// element is the output pattern, exactly as ffmpeg receives it).
fn stub_segmenter(dir: &Path) -> String {
    let path = dir.join("stub-segmenter.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nfor last in \"$@\"; do :; done\n\
         exec cat > \"$(dirname \"$last\")/pixel9-20260904T190000.opus\"\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.display().to_string()
}

#[test]
fn a_connection_lands_audio_liveness_and_evidence() {
    let root = tempfile::tempdir().unwrap();
    common::create_schema(root.path());
    let config = CaptureConfig {
        program: stub_segmenter(root.path()),
        ..CaptureConfig::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (server_side, _) = listener.accept().unwrap();
    let server = std::thread::spawn({
        let root = root.path().to_owned();
        move || {
            let dropped = AtomicBool::new(false);
            handle_connection(&server_side, &root, &config, &dropped);
        }
    });

    let pcm: Vec<u8> = [0i16, 400, -400, 0]
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    client
        .write_all(b"{\"id\":\"pixel9\",\"rate\":48000,\"channels\":1}\n")
        .unwrap();
    client.write_all(&pcm).unwrap();
    drop(client); // device disconnects
    server.join().unwrap();

    // The PCM reached the segmenter byte for byte.
    let segment = root.path().join("pixel9/pixel9-20260904T190000.opus");
    assert_eq!(std::fs::read(segment).unwrap(), pcm);
    // Audible signal refreshed the liveness marker.
    assert!(root.path().join("pixel9/.alive").exists());

    let conn = rusqlite::Connection::open(root.path().join("recall.sqlite")).unwrap();
    let kind: String = conn
        .query_row("SELECT kind FROM sources WHERE id = 'pixel9'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kind, "tcp_pcm");
    let events: Vec<(String, Option<String>)> = conn
        .prepare("SELECT kind, detail FROM capture_events ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, "ingest_connect");
    assert_eq!(events[1].0, "ingest_disconnect");
    // The disconnect record carries what the device actually sent.
    let stats: serde_json::Value = serde_json::from_str(events[1].1.as_deref().unwrap()).unwrap();
    assert_eq!(stats["bytes"], 8);
    assert_eq!(stats["ended"], "device disconnected");
    assert!(stats["peak_db"].as_f64().unwrap() < 0.0);
}

#[test]
fn a_malformed_handshake_leaves_no_trace() {
    let root = tempfile::tempdir().unwrap();
    common::create_schema(root.path());
    let config = CaptureConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (server_side, _) = listener.accept().unwrap();
    client.write_all(b"not a handshake\n").unwrap();
    drop(client);
    let dropped = AtomicBool::new(false);
    handle_connection(&server_side, root.path(), &config, &dropped);
    let conn = rusqlite::Connection::open(root.path().join("recall.sqlite")).unwrap();
    let events: i64 = conn
        .query_row("SELECT count(*) FROM capture_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 0);
}
