//! Per-source liveness, and its parity with the Python it replaces.

use chrono::{DateTime, Duration, Utc};
use recalld::sources::{
    Evidence, SourceKind, SourceRow, SourceStatus, active_window, source_statuses,
};
use std::collections::HashMap;

/// A one-shot HTTP agent: **no connection pooling**.
///
/// ⚠ `ureq::get`/`ureq::post` use ureq's GLOBAL agent, whose pool is shared by
/// every test in the binary — and the tests run in parallel against
/// short-lived per-test servers. When one test's server drops a socket another
/// test is returning to the pool, ureq panics inside the return path:
///
///     returning stream to pool: Os { code: 22, kind: InvalidInput }
///
/// That is the intermittent gate failure #1480 has been chasing: it needs two
/// tests' sockets to overlap, so it fires under load and never in a rerun. A
/// fresh agent with no idle connections removes the shared pool, and with it
/// the entire class — there is no socket to hand back.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().max_idle_connections(0).build()
}

fn now() -> DateTime<Utc> {
    // 2026-09-09T12:00:00Z — the instant the Python case matrix was generated at.
    DateTime::from_timestamp(1_788_998_400, 0).expect("a real instant")
}

fn ago(seconds: i64) -> DateTime<Utc> {
    now() - Duration::seconds(seconds)
}

fn sources() -> Vec<SourceRow> {
    vec![
        SourceRow {
            id: "usb".into(),
            name: "USB mic".into(),
            kind: SourceKind::CoreAudio,
        },
        SourceRow {
            id: "pixel5".into(),
            name: "Pixel 5".into(),
            kind: SourceKind::TcpPcm,
        },
        SourceRow {
            id: "geb".into(),
            name: "geb".into(),
            kind: SourceKind::Rtsp,
        },
    ]
}

struct Case {
    label: &'static str,
    markers: &'static [(&'static str, i64)],
    delivered: &'static [(&'static str, i64, Option<i64>)],
    on_fleet: bool,
    expect: &'static [(&'static str, bool, bool)],
}

/// ⚠ **Every expected value here was produced by the PYTHON**, not by this
/// implementation: `recall.liveness.source_statuses` was run on the identical
/// case matrix and its answers generated this table. Both sides of this function
/// are pure, which is what makes the comparison exact rather than approximate —
/// so a rule that reads the same and behaves differently fails here.
const CASES: &[Case] = &[
    Case {
        label: "all fresh markers",
        markers: &[("usb", 3), ("pixel5", 3), ("geb", 3)],
        delivered: &[],
        on_fleet: false,
        expect: &[
            ("usb", true, true),
            ("pixel5", true, true),
            ("geb", true, true),
        ],
    },
    Case {
        label: "phone marker 6s stale",
        markers: &[("pixel5", 6)],
        delivered: &[],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", false, false),
            ("geb", false, false),
        ],
    },
    Case {
        label: "phone 6s, on fleet",
        markers: &[("pixel5", 6)],
        delivered: &[],
        on_fleet: true,
        expect: &[
            ("usb", false, false),
            ("pixel5", true, true),
            ("geb", false, false),
        ],
    },
    Case {
        label: "mic 80s stale",
        markers: &[("usb", 80)],
        delivered: &[],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", false, false),
            ("geb", false, false),
        ],
    },
    Case {
        label: "mic 80s, on fleet",
        markers: &[("usb", 80)],
        delivered: &[],
        on_fleet: true,
        expect: &[
            ("usb", true, true),
            ("pixel5", false, false),
            ("geb", false, false),
        ],
    },
    Case {
        label: "geb delivery only",
        markers: &[],
        delivered: &[("geb", 60, Some(60))],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", false, false),
            ("geb", true, true),
        ],
    },
    Case {
        label: "geb delivered no speech",
        markers: &[],
        delivered: &[("geb", 60, None)],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", false, false),
            ("geb", false, true),
        ],
    },
    Case {
        label: "geb stale delivery",
        markers: &[],
        delivered: &[("geb", 400, Some(400))],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", false, false),
            ("geb", false, false),
        ],
    },
    Case {
        label: "stopped recently beats delivery",
        markers: &[("pixel5", 30)],
        delivered: &[("pixel5", 10, Some(10))],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", false, false),
            ("geb", false, false),
        ],
    },
    Case {
        label: "stopped long ago, delivery wins",
        markers: &[("pixel5", 400)],
        delivered: &[("pixel5", 10, Some(10))],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", true, true),
            ("geb", false, false),
        ],
    },
    Case {
        label: "marker fresh + old delivery",
        markers: &[("pixel5", 2)],
        delivered: &[("pixel5", 400, Some(400))],
        on_fleet: false,
        expect: &[
            ("usb", false, false),
            ("pixel5", true, true),
            ("geb", false, false),
        ],
    },
];

#[test]
fn every_rule_agrees_with_the_python_on_the_same_inputs() {
    for case in CASES {
        let markers: HashMap<String, DateTime<Utc>> = case
            .markers
            .iter()
            .map(|(id, off)| ((*id).to_owned(), ago(*off)))
            .collect();
        let delivered: HashMap<String, Evidence> = case
            .delivered
            .iter()
            .map(|(id, d, s)| {
                (
                    (*id).to_owned(),
                    Evidence {
                        delivered: Some(ago(*d)),
                        speech: s.map(ago),
                    },
                )
            })
            .collect();

        let got = source_statuses(&sources(), &markers, now(), case.on_fleet, &delivered);
        let by_id: HashMap<&str, &SourceStatus> =
            got.iter().map(|s| (s.source_id.as_str(), s)).collect();

        for (id, active, recording) in case.expect {
            let s = by_id
                .get(id)
                .unwrap_or_else(|| panic!("{}: no {id}", case.label));
            assert_eq!(s.active, *active, "{}: {id}.active", case.label);
            assert_eq!(s.recording, *recording, "{}: {id}.recording", case.label);
        }
    }
}

#[test]
fn the_fleet_lag_widens_every_window_by_the_report_cadence() {
    // The fleet sees markers one mirror report late, so a phone that is fine
    // locally at 6 s would read dead there without the widening.
    assert_eq!(
        active_window(SourceKind::TcpPcm, false),
        Duration::seconds(5)
    );
    assert_eq!(
        active_window(SourceKind::TcpPcm, true),
        Duration::seconds(12)
    );
    assert_eq!(
        active_window(SourceKind::CoreAudio, false),
        Duration::seconds(75)
    );
    assert_eq!(
        active_window(SourceKind::CoreAudio, true),
        Duration::seconds(82)
    );
    // Every non-streaming kind takes the watchdog window, not just the mic.
    assert_eq!(
        active_window(SourceKind::Rtsp, false),
        Duration::seconds(75)
    );
}

#[test]
fn only_real_recorders_are_devices() {
    // ⚠ An UPLOAD is a clip somebody sent and a DISCOVERED source is audio with
    // no known producer — asking whether either is "up" is asking about a
    // machine that may not exist.
    assert!(SourceKind::CoreAudio.is_device());
    assert!(SourceKind::TcpPcm.is_device());
    assert!(SourceKind::Rtsp.is_device());
    assert!(SourceKind::Lavfi.is_device());
    assert!(!SourceKind::Upload.is_device());
    assert!(!SourceKind::Discovered.is_device());
}

#[test]
fn the_two_time_fields_take_different_pairs() {
    // ⚠ lastActive is max(marker, SPEECH); lastDelivered is max(marker,
    // DELIVERED). Feeding both the same pair is the easy mistake, and it hides
    // exactly the case #1428 is about: a recorder shipping bytes nobody spoke
    // into.
    let markers = HashMap::new();
    let delivered = HashMap::from([(
        "geb".to_owned(),
        Evidence {
            delivered: Some(ago(60)),
            speech: Some(ago(600)),
        },
    )]);
    let got = source_statuses(&sources(), &markers, now(), false, &delivered);
    let geb = got.iter().find(|s| s.source_id == "geb").expect("geb");

    assert_eq!(
        geb.last_delivered,
        Some(ago(60)),
        "delivered is the newer one"
    );
    assert_eq!(
        geb.last_active,
        Some(ago(600)),
        "active takes SPEECH, not delivery"
    );
    assert!(geb.recording, "it is shipping bytes");
    assert!(!geb.active, "but nobody has been audible for ten minutes");
}

// --- the route, on the fleet ------------------------------------------------

use recalld::sources::fleet_sources;
use rusqlite::Connection;

/// A data root with both databases and the schema the route reads.
fn root_with(sources: &[(&str, &str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::store::open(&root).expect("ingest db");
    let conn = recalld::work::open_write(&root).expect("recall db");
    conn.execute_batch(
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);",
    )
    .expect("schema");
    for (id, name, kind) in sources {
        conn.execute("INSERT INTO sources VALUES (?1, ?2, ?3)", [id, name, kind])
            .expect("source");
    }
    drop(conn);
    (dir, root)
}

fn liveness(pairs: &[(&str, DateTime<Utc>)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(id, when)| {
            (
                (*id).to_owned(),
                serde_json::Value::String(recalld::instant::python_isoformat_utc(*when)),
            )
        })
        .collect()
}

fn active_by_id(
    root: &std::path::Path,
    conn: &Connection,
    at: DateTime<Utc>,
) -> HashMap<String, bool> {
    fleet_sources(root, conn, at)
        .expect("sources")
        .items
        .into_iter()
        .map(|s| (s.id, s.active))
        .collect()
}

/// ⚠ The fleet runs no capture agent and no phone sockets — the `.alive` markers
/// live on the Mac. Liveness must therefore come from the Mac's mirror report,
/// never from local files. The bug this pins: `/api/sources` once read
/// host-local state Isis cannot see and showed every microphone dead.
#[tokio::test]
async fn on_the_fleet_liveness_comes_from_the_macs_report() {
    let (_dir, root) = root_with(&[
        ("usb", "usb", "coreaudio"),
        ("pixel9", "Pixel 9", "tcp_pcm"),
        // An uploaded recording is a source but NOT a live device.
        ("meeting-x", "Meeting", "upload"),
    ]);
    let conn = recalld::work::open_write(&root).expect("db");
    let at = now();

    // Reported running, but no measured liveness shipped: nothing reads live —
    // a running agent is not proof of recording.
    recalld::capture::record_reported(&conn, at, true, None, &liveness(&[])).unwrap();
    let empty = active_by_id(&root, &conn, at);
    assert_eq!(empty.get("usb"), Some(&false));
    assert_eq!(empty.get("pixel9"), Some(&false));
    assert!(!empty.contains_key("meeting-x"), "uploads are not devices");

    // The Mac's next report ships both sources' fresh marker times.
    recalld::capture::record_reported(
        &conn,
        at,
        true,
        None,
        &liveness(&[("pixel9", at), ("usb", at)]),
    )
    .unwrap();
    let live = active_by_id(&root, &conn, at);
    assert_eq!(live.get("usb"), Some(&true));
    assert_eq!(live.get("pixel9"), Some(&true));

    // ⚠ When the Mac reports capture PAUSED, the mic reads idle AT ONCE — even
    // though its marker is still fresh and its window is a leisurely 75 s. A
    // pause that took a poll to show would be a pause the panel lied about.
    let until = recalld::instant::python_isoformat_utc(at + Duration::hours(1));
    recalld::capture::record_reported(&conn, at, false, Some(&until), &liveness(&[("usb", at)]))
        .unwrap();
    let paused = active_by_id(&root, &conn, at);
    assert_eq!(paused.get("usb"), Some(&false));
}

/// ⚠ A report older than the freshness gate means the Mac has stopped checking
/// in — the fleet genuinely does not know, and must say so rather than serving
/// the last thing it heard as if it were current.
#[tokio::test]
async fn a_stale_report_reads_as_nobody_live_not_as_the_last_thing_heard() {
    let (_dir, root) = root_with(&[("pixel9", "Pixel 9", "tcp_pcm")]);
    let conn = recalld::work::open_write(&root).expect("db");
    let at = now();

    recalld::capture::record_reported(&conn, at, true, None, &liveness(&[("pixel9", at)])).unwrap();
    assert_eq!(active_by_id(&root, &conn, at).get("pixel9"), Some(&true));

    // 31 s later the report itself has aged out of the 30 s gate.
    let later = at + Duration::seconds(31);
    assert_eq!(
        active_by_id(&root, &conn, later).get("pixel9"),
        Some(&false)
    );
}

/// An unknown kind must fail LOUD rather than becoming a row nothing matches.
#[test]
fn an_unknown_source_kind_is_an_error_not_a_silent_skip() {
    let (_dir, root) = root_with(&[("odd", "Odd", "telepathy")]);
    let conn = recalld::work::open_write(&root).expect("db");

    assert!(recalld::sources::source_rows(&conn).is_err());
}

/// ⚠ Written because of a real incident: on 2026-09-07 `/api/correct` was
/// written, tested, its Python deleted, and mounted NOWHERE — both suites green,
/// the endpoint served by nobody. And this route has a second way to be
/// unreachable: it is DEVICE-EXEMPT, so a gate that demanded a session here
/// would blank the recording panel on every phone in the house.
#[tokio::test]
async fn the_route_is_mounted_and_answers_without_a_session() {
    let (_dir, root) = root_with(&[("pixel9", "Pixel 9", "tcp_pcm")]);
    let conn = recalld::work::open_write(&root).expect("db");
    recalld::capture::record_reported(
        &conn,
        chrono::Utc::now(),
        true,
        None,
        &liveness(&[("pixel9", chrono::Utc::now())]),
    )
    .unwrap();
    drop(conn);

    let app = recalld::app::router(std::sync::Arc::new(recalld::app::Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: recalld::app::DEFAULT_MAX_BODY,
        // NOT None: the browsing plane is only mounted when SSO is configured,
        // so a test passing None would assert against a router without it.
        webauth: Some(recalld::webauth::GateState {
            cfg: std::sync::Arc::new(recalld::webauth::Config {
                session_secret: "test-secret-not-a-real-one".into(),
                client_id: "cid".into(),
                client_secret: "csec".into(),
                nc_base_url: "https://dash.example.org".into(),
                nc_internal_url: "https://dash.example.org".into(),
                redirect_uri: "http://127.0.0.1/auth/callback".into(),
                allowed_users: std::collections::HashSet::new(),
                device_token: None,
            }),
            now: std::sync::Arc::new(|| 1_788_000_000),
        }),
        sync_token: None,
        upstream: None,
        frontend: None,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // NO session cookie, deliberately.
    let body = tokio::task::spawn_blocking(move || {
        agent()
            .get(&format!("http://{addr}/api/sources"))
            .call()
            .expect("the route is mounted AND ungated")
            .into_string()
            .expect("body")
    })
    .await
    .expect("request");

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    let items = parsed["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "one device source");
    assert_eq!(items[0]["id"], "pixel9");
    assert_eq!(items[0]["kind"], "tcp_pcm");
    assert_eq!(items[0]["active"], true);
    // The camelCase keys are the frontend's contract, not a style choice.
    assert!(items[0].get("lastActive").is_some());
    assert!(items[0].get("lastDelivered").is_some());
}

/// Put a delivered segment in the ingest database, so the route finds real
/// evidence rather than a mocked mapping.
fn deliver(root: &std::path::Path, source: &str, captured: DateTime<Utc>, speech_s: f64) {
    let conn = recalld::store::open(root).expect("ingest db");
    let name = format!("{source}-{}.flac", captured.format("%Y%m%dT%H%M%S"));
    let stamp = captured.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    conn.execute(
        "INSERT INTO segments (filename, source, start_utc, bytes, sha256, received_utc)
         VALUES (?1, ?2, ?3, 1, 'x', ?3)",
        rusqlite::params![name, source, stamp],
    )
    .expect("segment");
    recalld::speech::ensure_schema(&conn).expect("speech schema");
    conn.execute(
        "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![name, source, speech_s, stamp],
    )
    .expect("speech");
}

/// ⚠ **A pause must read idle AT ONCE, and delivered evidence must not undo
/// it.** Delivery proof is up to a segment old, so audio captured in the seconds
/// BEFORE a pause would otherwise hold a dot green for the whole five-minute
/// delivered window — the exact opposite of the promise a pause makes.
///
/// This is asserted for a streaming phone as well as the local mic: a pause
/// stops the phones and the machines too, so none of them may be resurrected by
/// what they recorded just before it.
#[tokio::test]
async fn a_pause_silences_delivered_evidence_for_every_kind() {
    let (_dir, root) = root_with(&[("usb", "usb", "coreaudio"), ("geb", "geb", "tcp_pcm")]);
    let conn = recalld::work::open_write(&root).expect("db");
    let at = now();
    // Both delivered a speech-bearing segment 30 s ago — well inside the window.
    deliver(&root, "usb", at - Duration::seconds(30), 12.0);
    deliver(&root, "geb", at - Duration::seconds(30), 12.0);

    // While RUNNING, that evidence is what proves a store-and-forward recorder.
    recalld::capture::record_reported(&conn, at, true, None, &liveness(&[])).unwrap();
    let running = active_by_id(&root, &conn, at);
    assert_eq!(
        running.get("geb"),
        Some(&true),
        "delivery proves it while running"
    );
    assert_eq!(running.get("usb"), Some(&true));

    // The household pauses. Same evidence, and now it must prove nothing.
    let until = recalld::instant::python_isoformat_utc(at + Duration::hours(1));
    recalld::capture::record_reported(&conn, at, false, Some(&until), &liveness(&[])).unwrap();
    let paused = active_by_id(&root, &conn, at);
    assert_eq!(paused.get("usb"), Some(&false), "the mic is idle at once");
    assert_eq!(paused.get("geb"), Some(&false), "and so is the phone");
}

/// A store-and-forward recorder streams to nothing, so no marker of its is ever
/// refreshed — it reads dead while recording perfectly unless delivery counts
/// (#1428, geb after the C3 cutover).
#[tokio::test]
async fn a_recorder_that_streams_to_nothing_proves_itself_by_delivering() {
    let (_dir, root) = root_with(&[("geb", "geb", "rtsp")]);
    let conn = recalld::work::open_write(&root).expect("db");
    let at = now();
    // Running, and the Mac reports NO marker for geb — it never streams.
    recalld::capture::record_reported(&conn, at, true, None, &liveness(&[])).unwrap();
    assert_eq!(active_by_id(&root, &conn, at).get("geb"), Some(&false));

    deliver(&root, "geb", at - Duration::seconds(60), 9.0);

    assert_eq!(
        active_by_id(&root, &conn, at).get("geb"),
        Some(&true),
        "a delivered speech-bearing segment is the only proof it can offer"
    );
}
