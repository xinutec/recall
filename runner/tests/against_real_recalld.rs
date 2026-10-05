//! The runner against the real recalld router and a real shim subprocess.
//! Only the model is substituted, by a stub shim speaking the real protocol;
//! the queue, blob store, auth gate, client and stdio driver are the shipped ones.

use recalld::app::{Config as ServerConfig, router};
use runner::client::Client;
use runner::shim::{self, Shim};
use std::path::Path;
use std::sync::Arc;

const READ_TOKEN: &str = "read-me";

fn serve(root: &Path) -> String {
    // The lease reads a session's pinned language from the meaning plane.
    recalld::meaning_schema::ensure(
        &rusqlite::Connection::open(root.join("recall.sqlite")).expect("meaning"),
    )
    .expect("schema");
    let config = Arc::new(ServerConfig {
        root: root.to_owned(),
        tokens: None,
        read_token: Some(READ_TOKEN.to_owned()),
        max_body_bytes: 16 * 1024 * 1024,
        trusted_proxies: Vec::new(),
        webauth: None,
        sync_token: None,
        frontend: None,
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

/// Store a microphone clip with a queued transcription job.
fn queued_clip(root: &Path, name: &str, bytes: &[u8]) {
    let dir = root.join("ingest").join("usb");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), bytes).expect("blob");
    let conn = recalld::store::open(root).expect("db");
    recalld::store::insert(
        &conn,
        &recalld::store::Row {
            source: "usb".to_owned(),
            filename: name.to_owned(),
            start_utc: "2026-09-06T10:00:00Z".to_owned(),
            bytes: bytes.len() as u64,
            sha256: "x".to_owned(),
            received_utc: "2026-09-06T10:01:00Z".to_owned(),
            sent_utc: None,
        },
    )
    .expect("row");
    conn.execute(
        "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, '2026-09-06T10:01:00Z')",
        (audiocore::job::Kind::TranscribeSegment, name),
    )
    .expect("job");
}

/// A shim that runs `behaviour` for each request, and writes noise to stderr.
fn stub_shim(behaviour: &str) -> (String, Vec<String>) {
    let script = format!(
        "import json, sys\n\
         for line in sys.stdin:\n\
         \x20   line = line.strip()\n\
         \x20   if not line: continue\n\
         \x20   msg = json.loads(line)\n\
         \x20   print('noise on stdout is a real hazard', file=sys.stderr)\n\
         \x20   {behaviour}\n\
         \x20   sys.stdout.flush()\n"
    );
    ("python3".to_owned(), vec!["-c".to_owned(), script])
}

#[test]
fn a_job_is_leased_transcribed_and_acked() {
    let dir = tempfile::tempdir().expect("tempdir");
    queued_clip(dir.path(), "usb-20260906T100000.flac", b"audio bytes");
    let base = serve(dir.path());
    let client = Client::new(&base, READ_TOKEN);

    let (program, args) = stub_shim(
        "print(json.dumps({'id': msg['id'], 'ok': True, \
         'result': {'language': 'en', 'segments': [{'text': 'hello there'}]}}))",
    );
    let mut shim = Shim::spawn(&program, &args).expect("shim");

    let job = client
        .lease(&[audiocore::job::Kind::TranscribeSegment])
        .expect("lease")
        .expect("a job");
    assert_eq!(job.kind, audiocore::job::Kind::TranscribeSegment);
    assert_eq!(job.filename, "usb-20260906T100000.flac");

    let clip = dir.path().join("fetched.flac");
    client
        .fetch_blob("usb", &job.filename, &clip)
        .expect("blob");
    assert_eq!(std::fs::read(&clip).expect("read"), b"audio bytes");

    let result = shim.transcribe(&clip, None, None).expect("transcribe");
    assert_eq!(result.reply.segments[0].text, "hello there");

    client
        .finish(job.id, &result.raw.to_string())
        .expect("finish");
    // Not handed out again.
    assert!(
        client
            .lease(&[audiocore::job::Kind::TranscribeSegment])
            .expect("second lease")
            .is_none()
    );
}

#[test]
fn a_shim_refusal_is_reported_as_such_not_as_a_transport_failure() {
    // A refusal is recorded; a transport failure is retried.
    let dir = tempfile::tempdir().expect("tempdir");
    let (program, args) = stub_shim(
        "print(json.dumps({'id': msg['id'], 'ok': False, 'error': 'FileNotFoundError: x'}))",
    );
    let mut shim = Shim::spawn(&program, &args).expect("shim");
    let clip = dir.path().join("a.flac");
    std::fs::write(&clip, b"x").expect("clip");
    match shim.transcribe(&clip, None, None) {
        Err(shim::Error::Refused(why)) => assert!(why.contains("FileNotFoundError")),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_dead_shim_is_reported_as_closed_so_the_caller_can_respawn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let clip = dir.path().join("a.flac");
    std::fs::write(&clip, b"x").expect("clip");
    // Exits immediately: stdout closes with no answer.
    let mut shim = Shim::spawn("python3", &["-c".to_owned(), "pass".to_owned()]).expect("shim");
    match shim.transcribe(&clip, None, None) {
        Err(shim::Error::Closed | shim::Error::Write(_)) => {}
        other => panic!("expected a closed pipe, got {other:?}"),
    }
}

#[test]
fn model_and_prompt_reach_the_shim_when_given() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (program, args) =
        stub_shim("print(json.dumps({'id': msg['id'], 'ok': True, 'result': msg}))");
    let mut shim = Shim::spawn(&program, &args).expect("shim");
    let clip = dir.path().join("a.flac");
    std::fs::write(&clip, b"x").expect("clip");
    let echoed = shim
        .transcribe(&clip, Some("whisper-small"), Some("Oskar, Kat"))
        .expect("transcribe");
    assert_eq!(echoed.raw["op"], "transcribe");
    assert_eq!(echoed.raw["model"], "whisper-small");
    assert_eq!(echoed.raw["initial_prompt"], "Oskar, Kat");
    assert_eq!(echoed.raw["words"], true);
}

#[test]
fn the_vocabulary_prompt_is_read_and_an_empty_one_is_no_biasing() {
    // Empty is None: no `initial_prompt` at all.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let app = axum::Router::new()
                .route(
                    "/sync/vocabulary/prompt",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({"prompt": "Oskar, Kat"}))
                    }),
                )
                .route(
                    "/empty/sync/vocabulary/prompt",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({"prompt": null}))
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr")).expect("send");
            axum::serve(listener, app).await.expect("serve");
        });
    });
    let base = format!("http://{}", rx.recv().expect("addr"));

    let prompt = runner::client::Client::new(&base, "any")
        .prompt()
        .expect("fetch");
    assert_eq!(prompt.as_deref(), Some("Oskar, Kat"));

    let empty = runner::client::Client::new(&format!("{base}/empty"), "any")
        .prompt()
        .expect("fetch");
    assert_eq!(
        empty, None,
        "an empty vocabulary is no prompt, not an empty one"
    );
}

/// The doctor tells idle from dead by `rows == 0`. Drives the shipped binary.
#[test]
fn a_runner_with_an_empty_queue_stamps_a_beat_saying_it_had_nothing_to_do() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = serve(dir.path()); // no blobs registered: the queue derives nothing
    let pulse = dir.path().join("worker-heartbeat.json");
    // Named `voices`, so the runner skips the vocabulary fetch.
    let (program, args) =
        stub_shim("print(json.dumps({'id': msg['id'], 'ok': True, 'result': {'shim': 'voices'}}))");

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_runner"))
        .args(["--url", &base, "--once"])
        .arg("--pulse")
        .arg(&pulse)
        .arg("--shim")
        .arg(&program)
        .args(&args)
        .env("RECALL_SYNC_TOKEN", READ_TOKEN)
        .output()
        .expect("the runner runs");
    assert!(
        out.status.success(),
        "runner exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let raw = std::fs::read_to_string(&pulse).expect("a beat was stamped");
    let beat: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
    assert_eq!(
        beat["rows"].as_u64(),
        Some(0),
        "an empty queue is zero rows, not an absent beat"
    );
    assert!(
        beat["finished"].is_string(),
        "finished must be present, or the doctor reads a pass that never ended"
    );
}

#[test]
fn a_reply_outside_the_contract_is_refused_not_stored() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (program, args) = stub_shim(
        "print(json.dumps({'id': msg['id'], 'ok': True, 'result': {'segments': 'none'}}))",
    );
    let mut shim = Shim::spawn(&program, &args).expect("shim");
    let clip = dir.path().join("a.flac");
    std::fs::write(&clip, b"x").expect("clip");
    match shim.transcribe(&clip, None, None) {
        Err(shim::Error::Refused(why)) => assert!(why.contains("outside the contract"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}
