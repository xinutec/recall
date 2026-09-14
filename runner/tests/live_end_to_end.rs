//! The live agent as it actually runs: the real binary, the real tap socket,
//! the real recalld router, a real shim subprocess — only the model substituted.
//!
//! ⚠ **What this is for.** Every piece here is unit-tested already; what a unit
//! test cannot see is the WIRE. A live turn crosses a UDP socket, a stdio
//! protocol and an HTTP body with a field-naming convention on it, and the
//! failure mode of getting any of those wrong is not a crash — it is an agent
//! that runs, logs nothing alarming, and puts no turn on the timeline. This
//! asserts the ROW, at the far end.
//!
//! ⚠ **It never touches the household's tap.** `--tap` points at a port this
//! test owns: publishing a fixture onto 9876 would feed poetry to the live agent
//! on this machine, and reading 9876 would eat the datagrams it waits for.

use recalld::app::{Config as ServerConfig, router};
use std::net::UdpSocket;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOKEN: &str = "sync-me";
/// Datagram payload, matching what `audiod`'s segmenter publishes.
const PACKET: usize = 1316;

fn serve(root: &Path) -> String {
    let config = Arc::new(ServerConfig {
        root: root.to_owned(),
        tokens: None,
        read_token: Some(TOKEN.to_owned()),
        max_body_bytes: 16 * 1024 * 1024,
        webauth: None,
        sync_token: Some(TOKEN.to_owned()),
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

/// The meaning store the instant feed writes into. Its schema is Python's
/// (`store_schema.py`); this is the slice `ingest_live` touches.
fn meaning_store(root: &Path) {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("open");
    conn.execute_batch(
        "CREATE TABLE transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
             language TEXT, asr_model TEXT NOT NULL,
             superseded_by INTEGER, hidden_reason TEXT);
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');
         CREATE TABLE vocabulary (id INTEGER PRIMARY KEY, term TEXT NOT NULL);",
    )
    .expect("schema");
}

/// A shim that speaks the real protocol: names itself `asr` and answers every
/// clip with one line.
fn stub_shim() -> Vec<String> {
    vec![
        "-c".to_owned(),
        "import json, sys\n\
         for line in sys.stdin:\n\
         \x20   line = line.strip()\n\
         \x20   if not line: continue\n\
         \x20   msg = json.loads(line)\n\
         \x20   if msg.get('op') == 'hello':\n\
         \x20       out = {'ok': True, 'result': {'shim': 'asr'}}\n\
         \x20   else:\n\
         \x20       out = {'ok': True, 'result': {'language': 'en', 'segments': \
         [{'start': 0.0, 'end': 1.0, 'text': 'a stub heard something'}]}}\n\
         \x20   print(json.dumps(out), flush=True)\n"
            .to_owned(),
    ]
}

/// A port this test owns, so the household's tap is never read or written.
fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// Publish real speech onto the tap, paced like a microphone.
fn publish(port: u16, pcm: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
    let to = format!("127.0.0.1:{port}");
    for packet in pcm.chunks(PACKET) {
        // Unbounded speed would overflow the reader's fifo and drop the speech
        // this test is about. One packet is 41 ms of audio; pace it as such.
        std::thread::sleep(Duration::from_millis(20));
        let _ = socket.send_to(packet, &to);
    }
}

fn live_turns(root: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("open");
    let mut stmt = conn
        .prepare("SELECT start_utc, text FROM transcript_segments WHERE asr_model = 'live'")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

#[test]
fn speech_on_the_tap_becomes_a_turn_in_the_system_of_record() {
    let fixture = Path::new("../tests/fixtures/speech/public-domain-en.flac");
    assert!(fixture.exists(), "the committed fixture must not vanish");
    let pcm = audiocore::decode::decode_s16(fixture, audiocore::vad::RATE).expect("decode");
    // Enough of the reading to contain a whole utterance and a pause after it.
    let head: Vec<u8> = pcm
        .into_iter()
        .take(audiocore::vad::RATE as usize * 2 * 8)
        .collect();

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    meaning_store(root);
    let base = serve(root);
    let port = free_udp_port();

    let mut agent = std::process::Command::new(env!("CARGO_BIN_EXE_recall-live"))
        .args([
            "--url",
            &base,
            "--api",
            &base,
            "--tap",
            &format!("udp://127.0.0.1:{port}"),
            "--shim",
            "python3",
        ])
        .args(stub_shim())
        .env("RECALL_SYNC_TOKEN", TOKEN)
        .env("RUST_LOG", "info")
        // ⚠ NOT inherited, and this cost 34 minutes of a session. Killing the
        // agent leaves its ffmpeg orphaned — `Drop` cannot run on SIGKILL — and
        // an orphan holding an INHERITED stdout keeps the test harness's output
        // pipe open, so `cargo test` looks hung long after every test passed.
        // The orphan itself is bounded by the tap's own idle timeout; this makes
        // sure it cannot take the harness with it meanwhile.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the agent starts");

    // ffmpeg has to be listening before the first datagram: UDP drops what
    // arrives at a closed socket, and a test that raced would look like a
    // silent room.
    std::thread::sleep(Duration::from_secs(2));
    publish(port, &head);

    let deadline = Instant::now() + Duration::from_secs(30);
    let turns = loop {
        let turns = live_turns(root);
        if !turns.is_empty() || Instant::now() > deadline {
            break turns;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let _ = agent.kill();
    let _ = agent.wait();
    // The tap ffmpeg outlives the SIGKILL above; it exits on its own within the
    // idle timeout, and nothing here waits for that.

    assert!(
        !turns.is_empty(),
        "speech crossed the tap and no live turn reached the store"
    );
    assert_eq!(turns[0].1, "a stub heard something");
    // The spelling the rest of the system stores instants in — a second one is
    // how two rows for one turn happen.
    assert!(
        turns[0].0.ends_with("+00:00") && turns[0].0.contains('.'),
        "a live turn is stamped {}",
        turns[0].0
    );
}
