//! One device connection end to end, against a stub segmenter: handshake in,
//! PCM pumped through, liveness marked, the registration and both capture
//! events logged. The thin accept/pause loop is not driven here; its parts
//! have their own tests.

use audiod::segmenter::CaptureConfig;
use audiod::server::handle_connection;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// A segmenter stand-in that writes its stdin to a segment file beside the
/// output pattern (ffmpeg's last argument). It takes the extension from that
/// pattern, so the test sees the extension the configured codec asks for.
fn stub_segmenter(dir: &Path) -> String {
    let path = dir.join("stub-segmenter.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\nfor last in \"$@\"; do :; done\n\
         exec cat > \"$(dirname \"$last\")/pixel9-20260904T190000.${last##*.}\"\n",
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
    let segment = root.path().join("pixel9/pixel9-20260904T190000.flac");
    assert_eq!(std::fs::read(segment).unwrap(), pcm);
    // Audible signal refreshed the liveness marker.
    assert!(root.path().join("pixel9/.alive").exists());

    let events = audiocore::capture_log::read(root.path()).unwrap();
    let kinds: Vec<(&str, Option<&str>)> = events
        .iter()
        .map(|e| (e.kind.as_str(), e.detail.as_deref()))
        .collect();
    assert_eq!(
        kinds[..2],
        [("register", Some("tcp_pcm")), ("ingest_connect", None)]
    );
    assert_eq!(kinds.len(), 3);
    assert_eq!(kinds[2].0, "ingest_disconnect");
    // The disconnect record carries what the device actually sent.
    let stats: serde_json::Value = serde_json::from_str(kinds[2].1.unwrap()).unwrap();
    assert_eq!(stats["bytes"], 8);
    assert_eq!(stats["ended"], "device disconnected");
    assert!(stats["peak_db"].as_f64().unwrap() < 0.0);
}

#[test]
fn a_malformed_handshake_leaves_no_trace() {
    let root = tempfile::tempdir().unwrap();
    let config = CaptureConfig::default();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let (server_side, _) = listener.accept().unwrap();
    client.write_all(b"not a handshake\n").unwrap();
    drop(client);
    let dropped = AtomicBool::new(false);
    handle_connection(&server_side, root.path(), &config, &dropped);
    assert!(
        audiocore::capture_log::read(root.path())
            .unwrap()
            .is_empty()
    );
}
