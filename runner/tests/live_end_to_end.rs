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

/// The meaning store the instant feed writes into — built by the REAL migration
/// ladder, not a copy of it.
///
/// ⚠ A hand-written slice used to stand here, and it silently stopped matching
/// production the first time a column was added: the write failed, no turn was
/// stored, and this test reported that speech had not crossed the tap.
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

/// A port this test owns, so the household's tap is never read or written.
///
/// ⚠⚠ **Outside the EPHEMERAL range, and that is the whole point.** This binds
/// to find a free port, reads it, then DROPS the socket so ffmpeg can take it —
/// a real window in which anything may claim it. Drawn from `:0` that window is
/// inside `net.inet.ip.portrange.first..last`, which is exactly where the kernel
/// hands out ports to every other process on the machine, so two concurrent test
/// suites are two allocators racing for the same pool. #1630's leading
/// hypothesis, and the same class as #1480's.
///
/// Below the range nothing is allocated automatically, so only another copy of
/// THIS test could collide. The window is not closed — it cannot be, while
/// ffmpeg is the one that must bind — but it stops being a lottery everything
/// else on the machine is entered into.
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

/// Which half failed, when no turn arrived — the tap, or the store.
///
/// ⚠ **They are INDISTINGUISHABLE in this test's result, and that has already
/// cost real time.** Adding a column broke the schema this test used to
/// hand-copy: the write was rejected, no turn was stored, and the failure read
/// as "speech crossed the tap and no live turn reached the store" — the same
/// sentence the flake under investigation reports (#1630). A race was the
/// leading suspect for a failure that was not a race at all.
///
/// So the store half is probed with the REAL writer rather than a model of it:
/// if a canary goes in, the schema and the file are fine and the failure is
/// upstream of them. ⓘ The canary is only ever written on the failure path, and
/// the assertion that follows has already been decided by then.
fn which_half_failed(root: &Path, agent: &mut std::process::Child, elapsed: Duration) -> String {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite"));
    // ⚠ Counted BEFORE the canary, and labelled as such: "stored 1" beside a
    // count taken after it would read as a contradiction.
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
    // ⚠ `try_wait`, never `wait`: the agent is still running in the passing
    // case, and blocking here would hang the diagnosis instead of printing it.
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

    // ⚠ **UDP DROPS WHAT ARRIVES AT A CLOSED SOCKET, so a missed burst is a
    // RACE, not a slow path — and a longer deadline cannot wait for something
    // that was never sent.** This published ONCE after a flat 2-second sleep. It
    // passed in 7s on an idle machine and failed inside the gate on 2026-09-15,
    // burning the whole 30s deadline while another repository's gate ran: by the
    // time anything was listening the reading was over.
    //
    // So the reading is sent AGAIN until a turn appears. That is sound whatever
    // swallowed the first burst — a late ffmpeg, a stolen port, a stalled spawn
    // — which a readiness probe is not: binding the port ourselves to see if it
    // is taken reports "ready" just as confidently when the holder is some other
    // test's socket. ⚠ The cause of the gate failure is NOT established (#1630);
    // this makes a single dropped burst survivable, and claims nothing more.
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
    // ⚠ Diagnosed BEFORE the kill. `try_wait` on a killed process says only that
    // it is gone, which is the one answer that cannot distinguish anything.
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
    // The spelling the rest of the system stores instants in — a second one is
    // how two rows for one turn happen.
    assert!(
        turns[0].0.ends_with("+00:00") && turns[0].0.contains('.'),
        "a live turn is stamped {}",
        turns[0].0
    );
}
