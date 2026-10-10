//! The second queue: messages' voice messages, transcribed by the same Whisper
//! process when recall's own queue is empty. Messages serves the runner's four
//! routes, so a second real recalld stands in for it here, which is why its
//! file names follow recall's grammar. Drives the shipped binary.

use recalld::app::{Config as ServerConfig, router};
use std::path::Path;
use std::process::Output;
use std::sync::Arc;

const RECALL_TOKEN: &str = "recall-token";
const MESSAGES_TOKEN: &str = "messages-token";

fn serve(root: &Path, token: &str) -> String {
    recalld::meaning_schema::ensure(
        &rusqlite::Connection::open(root.join("recall.sqlite")).expect("meaning"),
    )
    .expect("schema");
    recalld::store::open(root).expect("ingest db");
    let config = Arc::new(ServerConfig {
        root: root.to_owned(),
        tokens: None,
        read_token: Some(token.to_owned()),
        max_body_bytes: 16 * 1024 * 1024,
        trusted_proxies: Vec::new(),
        webauth: None,
        sync_token: Some(token.to_owned()),
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

fn queued_clip(root: &Path, source: &str, name: &str) {
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), b"audio bytes").expect("blob");
    let conn = recalld::store::open(root).expect("db");
    recalld::store::insert(
        &conn,
        &recalld::store::Row {
            source: source.to_owned(),
            filename: name.to_owned(),
            start_utc: "2026-10-10T10:00:00Z".to_owned(),
            bytes: 11,
            sha256: "x".to_owned(),
            received_utc: "2026-10-10T10:01:00Z".to_owned(),
            sent_utc: None,
        },
    )
    .expect("row");
    conn.execute(
        "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, '2026-10-10T10:01:00Z')",
        (audiocore::job::Kind::TranscribeSegment, name),
    )
    .expect("job");
}

/// The job's state and stored result.
fn job(root: &Path, name: &str) -> (String, Option<String>) {
    rusqlite::Connection::open(root.join("ingest.sqlite"))
        .expect("db")
        .query_row(
            "SELECT state, result FROM jobs WHERE filename = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("job row")
}

/// An `asr` shim whose transcript is the vocabulary prompt it was given.
fn shim() -> Vec<String> {
    let script = "import json, sys\n\
         for line in sys.stdin:\n\
         \x20   msg = json.loads(line)\n\
         \x20   if msg['op'] == 'hello':\n\
         \x20       result = {'shim': 'asr'}\n\
         \x20   else:\n\
         \x20       result = {'language': 'en', 'segments': [{'text': 'prompt=' + str(msg.get('initial_prompt'))}]}\n\
         \x20   print(json.dumps({'id': msg['id'], 'ok': True, 'result': result}))\n\
         \x20   sys.stdout.flush()\n";
    vec!["python3".to_owned(), "-c".to_owned(), script.to_owned()]
}

fn run_once(recall: &str, messages: &str, pulse: &Path, messages_token: Option<&str>) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_runner"));
    cmd.args(["--url", recall, "--once", "--messages-url", messages])
        .arg("--pulse")
        .arg(pulse)
        .arg("--shim")
        .args(shim())
        .env("RECALL_SYNC_TOKEN", RECALL_TOKEN)
        .env_remove("MESSAGES_TRANSCRIBER_TOKEN");
    if let Some(token) = messages_token {
        cmd.env("MESSAGES_TRANSCRIBER_TOKEN", token);
    }
    cmd.output().expect("the runner runs")
}

fn succeeded(out: &Output) {
    assert!(
        out.status.success(),
        "runner exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two servers, recall's with a vocabulary so a leaked prompt shows.
fn servers() -> (tempfile::TempDir, String, tempfile::TempDir, String) {
    let recall = tempfile::tempdir().expect("tempdir");
    let messages = tempfile::tempdir().expect("tempdir");
    let recall_url = serve(recall.path(), RECALL_TOKEN);
    let messages_url = serve(messages.path(), MESSAGES_TOKEN);
    rusqlite::Connection::open(recall.path().join("recall.sqlite"))
        .expect("meaning")
        .execute(
            "INSERT INTO vocabulary (term, created_utc) VALUES ('Hexwick', '2026-10-10T00:00:00+00:00')",
            [],
        )
        .expect("term");
    (recall, recall_url, messages, messages_url)
}

#[test]
fn with_nothing_of_its_own_it_transcribes_a_message_unbiased_and_answers_messages() {
    let (recall, recall_url, messages, messages_url) = servers();
    queued_clip(messages.path(), "chat", "chat-20261010T100000.m4a");
    let pulse = recall.path().join("worker-heartbeat.json");

    succeeded(&run_once(
        &recall_url,
        &messages_url,
        &pulse,
        Some(MESSAGES_TOKEN),
    ));

    let (state, result) = job(messages.path(), "chat-20261010T100000.m4a");
    assert_eq!(state, "done");
    let result = result.expect("a stored result");
    assert!(
        result.contains("prompt=None"),
        "recall's vocabulary must not reach another server's audio: {result}"
    );
    // The pulse is recall's: an empty queue of its own is zero rows.
    let beat: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&pulse).expect("beat")).expect("json");
    assert_eq!(beat["rows"].as_u64(), Some(0));
}

#[test]
fn household_audio_goes_first() {
    let (recall, recall_url, messages, messages_url) = servers();
    queued_clip(recall.path(), "usb", "usb-20261010T100000.flac");
    queued_clip(messages.path(), "chat", "chat-20261010T100000.m4a");
    let pulse = recall.path().join("worker-heartbeat.json");

    succeeded(&run_once(
        &recall_url,
        &messages_url,
        &pulse,
        Some(MESSAGES_TOKEN),
    ));

    let (state, result) = job(recall.path(), "usb-20261010T100000.flac");
    assert_eq!(state, "done");
    assert!(result.expect("result").contains("prompt=Hexwick"));
    assert_eq!(job(messages.path(), "chat-20261010T100000.m4a").0, "queued");
}

#[test]
fn a_messages_outage_is_not_a_failed_run() {
    let (recall, recall_url, _messages, _url) = servers();
    // Nothing listens here.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        format!("http://{}", listener.local_addr().expect("addr"))
    };
    let pulse = recall.path().join("worker-heartbeat.json");

    let out = run_once(&recall_url, &closed, &pulse, Some(MESSAGES_TOKEN));

    succeeded(&out);
    assert!(pulse.exists(), "recall's own pass still stamps its beat");
}

#[test]
fn messages_refuses_recalls_token() {
    // Run with the wrong token: messages' job must stay queued, and the run
    // must not fail because of it.
    let (recall, recall_url, messages, messages_url) = servers();
    queued_clip(messages.path(), "chat", "chat-20261010T100000.m4a");
    let pulse = recall.path().join("worker-heartbeat.json");

    succeeded(&run_once(
        &recall_url,
        &messages_url,
        &pulse,
        Some(RECALL_TOKEN),
    ));

    assert_eq!(job(messages.path(), "chat-20261010T100000.m4a").0, "queued");
}

#[test]
fn the_messages_queue_without_its_token_is_refused_at_start() {
    let (recall, recall_url, _messages, messages_url) = servers();
    let pulse = recall.path().join("worker-heartbeat.json");

    let out = run_once(&recall_url, &messages_url, &pulse, None);

    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("MESSAGES_TRANSCRIBER_TOKEN"));
}

#[test]
fn a_job_names_a_file_in_the_scratch_directory_or_nothing() {
    // The name comes from the server; it must not write outside scratch.
    let job = |filename: &str| -> runner::client::Job {
        serde_json::from_value(serde_json::json!({
            "id": 1, "kind": "transcribe-segment", "filename": filename, "source": "chat"
        }))
        .expect("job")
    };
    assert_eq!(job("chat-1.m4a").scratch_name(), Some("chat-1.m4a"));
    for unsafe_name in ["../escape.m4a", "a/b.m4a", "/etc/passwd", "..", ".", ""] {
        assert_eq!(job(unsafe_name).scratch_name(), None, "{unsafe_name:?}");
    }
}
