//! The live agent as it actually runs: the real binary, the real tap socket,
//! the real recalld router, a real shim subprocess; only the model is substituted.
//!
//! A live turn crosses a UDP socket, a stdio protocol and an HTTP body, and
//! getting any wrong gives an agent that runs quietly and stores no turn. So
//! this asserts the row at the far end.
//!
//! `--tap` points at a port this test owns: publishing onto the real tap (9876)
//! would feed the fixture to this machine's live agent, and reading it would
//! steal that agent's datagrams.

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

/// The meaning store the live feed writes into, built by the real migrations
/// so it cannot drift from production's schema.
fn meaning_store(root: &Path) {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("open");
    recalld::meaning_schema::ensure(&conn).expect("schema");
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

/// A free UDP port below the ephemeral range. The socket is dropped so ffmpeg
/// can bind it, leaving a window in which the port can be claimed; below the
/// ephemeral range the kernel never hands it to another process in that window.
fn free_udp_port() -> u16 {
    for port in 20_000..32_768 {
        if let Ok(socket) = UdpSocket::bind(("127.0.0.1", port)) {
            let bound = socket.local_addr().expect("addr").port();
            drop(socket);
            return bound;
        }
    }
    panic!("no free udp port outside the ephemeral range")
}

/// Publish real speech onto the tap, paced like a microphone.
fn publish(port: u16, pcm: &[u8]) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind");
    let to = format!("127.0.0.1:{port}");
    for packet in pcm.chunks(PACKET) {
        // Unpaced sending would overflow the reader's fifo. One packet is 41 ms
        // of audio, so 20 ms between packets stays ahead without flooding.
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

/// Which half failed when no turn arrived, the tap or the store; the test's
/// result alone cannot tell them apart. The store is probed with the real
/// writer: if a canary goes in, the failure is upstream. Only called on the
/// failure path, after the outcome is decided.
fn which_half_failed(root: &Path, agent: &mut std::process::Child, elapsed: Duration) -> String {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite"));
    // Counted before the canary, and labelled so.
    let rows = conn.as_ref().map_or_else(ToString::to_string, |c| {
        c.query_row("SELECT COUNT(*) FROM transcript_segments", [], |r| {
            r.get::<_, i64>(0)
        })
        .map_or_else(|e| e.to_string(), |n| n.to_string())
    });
    let canary = match rusqlite::Connection::open(root.join("recall.sqlite")) {
        Ok(mut c) => recalld::work::ingest_live(
            &mut c,
            &[recalld::work::LiveTurn {
                start: "2000-01-01T00:00:00+00:00".to_owned(),
                end: "2000-01-01T00:00:01+00:00".to_owned(),
                text: "a canary probing the write half".to_owned(),
                asr_model: "live".to_owned(),
                language: Some("en".to_owned()),
            }],
            chrono::Utc::now(),
        )
        .map_or_else(|e| format!("REFUSED: {e}"), |n| format!("stored {n}")),
        Err(err) => format!("cannot open the store: {err}"),
    };
    // `try_wait`, not `wait`: a still-running agent would hang the diagnosis.
    let alive = match agent.try_wait() {
        Ok(None) => "still running".to_owned(),
        Ok(Some(status)) => format!("EXITED {status}"),
        Err(err) => format!("unknown ({err})"),
    };
    format!(
        "\n  the store half: {rows} rows before it; a canary write {canary}\
         \n  the tap half:   the agent is {alive}\
         \n  elapsed:        {:.1}s (a passing run takes ~7s; a failure sits in the whole deadline)",
        elapsed.as_secs_f64()
    )
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
            "--tap",
            &format!("udp://127.0.0.1:{port}"),
            "--shim",
            "python3",
        ])
        .args(stub_shim())
        .env("RECALL_SYNC_TOKEN", TOKEN)
        .env("RUST_LOG", "info")
        // Not inherited: killing the agent orphans its ffmpeg (`Drop` cannot
        // run on SIGKILL), and an orphan holding an inherited stdout keeps the
        // harness's pipe open so `cargo test` looks hung after every test passed.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the agent starts");

    // UDP drops what arrives before the socket is bound, so a longer deadline
    // cannot rescue a burst sent too early. The reading is resent until a turn
    // appears, which survives any cause of a lost burst; a readiness probe
    // could not tell our listener from another process holding the port.
    std::thread::sleep(Duration::from_secs(2));

    let started = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut turns = Vec::new();
    while turns.is_empty() && Instant::now() < deadline {
        publish(port, &head);
        // The utterance has to be cut, transcribed and pushed after the last
        // packet lands; poll for that before sending the reading again.
        let settle = Instant::now() + Duration::from_secs(10);
        while Instant::now() < settle {
            turns = live_turns(root);
            if !turns.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    // Diagnosed before the kill, since a killed process tells `try_wait` nothing.
    let halves = if turns.is_empty() {
        which_half_failed(root, &mut agent, started.elapsed())
    } else {
        String::new()
    };
    let _ = agent.kill();
    let _ = agent.wait();
    // The tap ffmpeg outlives the SIGKILL above; it exits on its own within the
    // idle timeout, and nothing here waits for that.

    assert!(!turns.is_empty(), "no live turn reached the store.{halves}");
    assert_eq!(turns[0].1, "a stub heard something");
    // The system's one spelling of an instant; a second would split one turn into two rows.
    assert!(
        turns[0].0.ends_with("+00:00") && turns[0].0.contains('.'),
        "a live turn is stamped {}",
        turns[0].0
    );
}
