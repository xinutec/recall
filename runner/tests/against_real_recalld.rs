//! The runner against the REAL recalld router and a REAL shim subprocess.
//!
//! Only the model is substituted — a stub shim speaking the actual protocol —
//! because a unit test has no business loading Whisper. Everything else is the
//! pair that ships: recalld's own queue, its own blob store, its own auth gate,
//! and the runner's own client and stdio driver.

use recalld::app::{Config as ServerConfig, router};
use runner::client::Client;
use runner::shim::{self, Shim};
use std::path::Path;
use std::sync::Arc;

const READ_TOKEN: &str = "read-me";

fn serve(root: &Path) -> String {
    let config = Arc::new(ServerConfig {
        root: root.to_owned(),
        tokens: None,
        read_token: Some(READ_TOKEN.to_owned()),
        max_body_bytes: 16 * 1024 * 1024,
        // The runner uses the work plane only; the browsing plane is absent here.
        webauth: None,
        sync_token: None,
        upstream: None,
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

/// Register a room blob so the queue derives a job for it.
fn room_segment(root: &Path, name: &str, bytes: &[u8]) {
    let dir = root.join("ingest").join("room");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), bytes).expect("blob");
    let conn = recalld::store::open(root).expect("db");
    recalld::store::insert(
        &conn,
        &recalld::store::Row {
            source: "room".to_owned(),
            filename: name.to_owned(),
            start_utc: "2026-09-06T10:00:00Z".to_owned(),
            bytes: bytes.len() as u64,
            sha256: "x".to_owned(),
            received_utc: "2026-09-06T10:01:00Z".to_owned(),
            sent_utc: None,
        },
    )
    .expect("row");
}

/// A shim that speaks the real protocol and answers whatever it is told to.
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
    room_segment(dir.path(), "room-20260906T100000.flac", b"audio bytes");
    let base = serve(dir.path());
    let client = Client::new(&base, READ_TOKEN);

    let (program, args) = stub_shim(
        "print(json.dumps({'id': msg['id'], 'ok': True, \
         'result': {'language': 'en', 'segments': [{'text': 'hello there'}]}}))",
    );
    let mut shim = Shim::spawn(&program, &args).expect("shim");

    let job = client
        .lease(&["transcribe-room"])
        .expect("lease")
        .expect("a job");
    assert_eq!(job.kind, "transcribe-room");
    assert_eq!(job.filename, "room-20260906T100000.flac");

    // The blob comes from recalld's own store, over its own auth gate.
    let clip = dir.path().join("fetched.flac");
    client
        .fetch_blob("room", &job.filename, &clip)
        .expect("blob");
    assert_eq!(std::fs::read(&clip).expect("read"), b"audio bytes");

    let result = shim.transcribe(&clip, None, None).expect("transcribe");
    assert_eq!(result["segments"][0]["text"], "hello there");

    client.finish(job.id, &result.to_string()).expect("finish");
    // Retiring is terminal: the queue must not hand the same job out again.
    assert!(
        client
            .lease(&["transcribe-room"])
            .expect("second lease")
            .is_none()
    );
}

#[test]
fn a_shim_refusal_is_reported_as_such_not_as_a_transport_failure() {
    // The distinction the runner acts on: a refusal is the CLIP's fault and is
    // recorded terminally; a transport failure is the SHIM's and is retried.
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
        .transcribe(&clip, Some("whisper-small"), Some("Pippijn, Kat"))
        .expect("transcribe");
    assert_eq!(echoed["op"], "transcribe");
    assert_eq!(echoed["model"], "whisper-small");
    assert_eq!(echoed["initial_prompt"], "Pippijn, Kat");
    assert_eq!(echoed["words"], true);
}

#[test]
fn the_vocabulary_prompt_is_read_and_an_empty_one_is_no_biasing() {
    // #1463: the runner carries the prompt because the shim may not fetch it.
    // An EMPTY vocabulary must read as None — "send no initial_prompt" — rather
    // than as an empty string, which would be an instruction to the model.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let app = axum::Router::new()
                .route(
                    "/sync/vocabulary/prompt",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({"prompt": "Pippijn, Kat"}))
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

    let prompt = runner::client::fetch_prompt(&base, "any").expect("fetch");
    assert_eq!(prompt.as_deref(), Some("Pippijn, Kat"));

    let empty = runner::client::fetch_prompt(&format!("{base}/empty"), "any").expect("fetch");
    assert_eq!(
        empty, None,
        "an empty vocabulary is NO biasing, not an empty prompt"
    );
}
