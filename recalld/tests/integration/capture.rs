//! The capture-control state machine, and the fingerprint every client's
//! long-poll depends on.

use audiocore::capture::{CaptureState, Window};
use chrono::{DateTime, Duration, Utc};
use recalld::capture::{
    self, Press, Whose, fleet_capture_state, intent_of, press, record_control_origin,
    reported_state,
};
use rusqlite::Connection;

fn at(offset_s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_788_894_682 + offset_s, 0).expect("a real instant")
}

/// `at(offset_s)` as a window's spelling of it.
fn iso(offset_s: i64) -> String {
    at(offset_s).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The meaning plane, whose settings table holds all this state.
fn store(rows: &[(&str, &str)]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    for (k, v) in rows {
        conn.execute("INSERT INTO settings VALUES (?1, ?2)", [k, v])
            .unwrap();
    }
    conn
}

fn base() -> CaptureState {
    CaptureState {
        running: false,
        until: None,
        held: false,
        desired_running: false,
        desired_until: None,
        desired_held: false,
        settled: true,
        mic_reachable: true,
        state_token: String::new(),
    }
}

#[test]
fn the_token_changes_when_any_graded_field_does() {
    let stamped = capture::stamped(base());
    let until = Some(iso(60));
    for other in [
        CaptureState {
            running: true,
            ..base()
        },
        CaptureState {
            until: until.clone(),
            ..base()
        },
        CaptureState {
            held: true,
            ..base()
        },
        CaptureState {
            desired_running: true,
            ..base()
        },
        CaptureState {
            desired_until: until.clone(),
            ..base()
        },
        CaptureState {
            desired_held: true,
            ..base()
        },
        CaptureState {
            settled: false,
            ..base()
        },
        CaptureState {
            mic_reachable: false,
            ..base()
        },
    ] {
        assert_ne!(
            stamped.state_token,
            capture::stamped(other.clone()).state_token,
            "a client would never see this change: {other:?}"
        );
    }
}

// --- reading the intent -----------------------------------------------------

#[test]
fn nobody_having_pressed_anything_is_off() {
    let conn = store(&[]);
    assert_eq!(
        intent_of(&conn, &Whose::House, at(0)).unwrap(),
        Some(Window::Off)
    );
    let state = fleet_capture_state(&conn, at(0)).unwrap();
    assert!(!state.desired_running);
}

#[test]
fn a_window_records_until_its_end_and_then_is_off() {
    let conn = store(&[("capture_intent", &format!("record {}", iso(60)))]);
    assert_eq!(
        intent_of(&conn, &Whose::House, at(0)).unwrap(),
        Some(Window::Record(at(60)))
    );
    assert_eq!(
        intent_of(&conn, &Whose::House, at(60)).unwrap(),
        Some(Window::Off)
    );
}

/// A stored intent that does not read is a fault, and a fault records.
#[test]
fn an_unparseable_intent_is_a_fault_that_records() {
    let conn = store(&[("capture_intent", "soon")]);
    assert_eq!(intent_of(&conn, &Whose::House, at(0)).unwrap(), None);
    let state = fleet_capture_state(&conn, at(0)).unwrap();
    assert!(state.desired_running);
    assert_eq!(
        state.desired_until, None,
        "recording with no end: the fault"
    );
}

// --- the Mac's confirmation -------------------------------------------------

fn reported(window: &str) -> Vec<(&'static str, String)> {
    vec![
        ("capture_reported_running", "1".to_owned()),
        ("capture_reported_window", window.to_owned()),
        ("capture_reported_at", iso(0)),
    ]
}

fn store_owned(rows: &[(&str, String)]) -> Connection {
    let rows: Vec<(&str, &str)> = rows.iter().map(|(k, v)| (*k, v.as_str())).collect();
    store(&rows)
}

#[test]
fn a_stale_report_means_the_mic_is_not_reachable() {
    // Past the 30 s freshness window, intent is shown as intent, never as
    // confirmation.
    let conn = store_owned(&reported("off"));
    assert!(reported_state(&conn, at(29)).unwrap().is_some());
    assert!(reported_state(&conn, at(31)).unwrap().is_none());

    let state = fleet_capture_state(&conn, at(31)).unwrap();
    assert!(!state.mic_reachable);
    assert!(!state.settled);
}

#[test]
fn a_confirmed_window_is_settled() {
    let window = format!("record {}", iso(60));
    let mut rows = reported(&window);
    rows.push(("capture_intent", window.clone()));
    let state = fleet_capture_state(&store_owned(&rows), at(1)).unwrap();
    assert!(state.running && state.desired_running);
    assert_eq!(state.until.as_deref(), Some(iso(60).as_str()));
    assert!(state.settled);
    assert!(state.mic_reachable);
}

#[test]
fn a_window_the_mac_has_not_applied_yet_is_unsettled() {
    // Renders as "Starting…": desired has flipped, confirmed lags.
    let mut rows = reported("off");
    rows.push(("capture_intent", format!("record {}", iso(60))));
    let state = fleet_capture_state(&store_owned(&rows), at(1)).unwrap();
    assert!(!state.running, "the mic's confirmed word is still off");
    assert!(state.desired_running);
    assert!(!state.settled);
}

#[test]
fn moving_the_end_reads_as_transitioning_until_applied() {
    // Both sides record; only the end differs, and that must not read settled.
    let mut rows = reported(&format!("record {}", iso(60)));
    rows.push(("capture_intent", format!("record {}", iso(120))));
    let state = fleet_capture_state(&store_owned(&rows), at(1)).unwrap();
    assert!(!state.settled, "the Mac is still on the OLD end");
    assert_eq!(state.until.as_deref(), Some(iso(60).as_str()));
    assert_eq!(state.desired_until.as_deref(), Some(iso(120).as_str()));
}

#[test]
fn a_window_both_sides_saw_end_is_settled_off() {
    // Nothing writes at the end: both read off from the same instant.
    let window = format!("record {}", iso(60));
    let mut rows = reported(&window);
    rows.push(("capture_intent", window.clone()));
    // Reported just before the end, read just after: still fresh.
    rows[2].1 = iso(50);
    let state = fleet_capture_state(&store_owned(&rows), at(61)).unwrap();
    assert!(!state.desired_running && !state.running);
    assert!(state.settled);
}

/// The Mac's gate not reading is its fault: it records, and says so.
#[test]
fn a_mac_whose_gate_does_not_read_reports_recording_with_no_end() {
    let mut rows = reported("");
    rows.push(("capture_intent", "off".to_owned()));
    let state = fleet_capture_state(&store_owned(&rows), at(1)).unwrap();
    assert!(state.running);
    assert_eq!(state.until, None);
    assert!(!state.settled, "off was asked for and is not applied");
}

// --- the write half: presses, and the audit of who asked --------------------

fn writable() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

#[test]
fn record_opens_a_window_capped_at_a_day() {
    let conn = writable();
    let opened = press(
        &conn,
        &Whose::House,
        at(0),
        Press::Record(at(0) + Duration::hours(30)),
    )
    .unwrap();
    assert_eq!(opened, Window::Record(at(0) + Duration::hours(24)));
    assert_eq!(
        intent_of(&conn, &Whose::House, at(0)).unwrap(),
        Some(opened)
    );
}

#[test]
fn record_until_a_time_already_past_is_off() {
    let conn = writable();
    assert_eq!(
        press(&conn, &Whose::House, at(0), Press::Record(at(-60))).unwrap(),
        Window::Off
    );
}

#[test]
fn pause_and_unpause_keep_the_end_and_stop_ends_it() {
    let conn = writable();
    press(&conn, &Whose::House, at(0), Press::Record(at(600))).unwrap();
    assert_eq!(
        press(&conn, &Whose::House, at(1), Press::Pause).unwrap(),
        Window::Hold(at(600))
    );
    let held = fleet_capture_state(&conn, at(1)).unwrap();
    assert!(!held.desired_running && held.desired_held);
    assert_eq!(
        press(&conn, &Whose::House, at(2), Press::Unpause).unwrap(),
        Window::Record(at(600))
    );
    assert_eq!(
        press(&conn, &Whose::House, at(3), Press::Stop).unwrap(),
        Window::Off
    );
}

#[test]
fn pausing_when_off_or_after_a_fault_stays_off() {
    let conn = writable();
    assert_eq!(
        press(&conn, &Whose::House, at(0), Press::Pause).unwrap(),
        Window::Off
    );
    assert_eq!(
        press(&conn, &Whose::House, at(0), Press::Unpause).unwrap(),
        Window::Off
    );
    let faulty = store(&[("capture_intent", "soon")]);
    assert_eq!(
        press(&faulty, &Whose::House, at(0), Press::Pause).unwrap(),
        Window::Off
    );
}

#[test]
fn one_persons_window_leaves_the_houses_alone() {
    let conn = writable();
    let her = Whose::Person("someone".into());
    press(&conn, &her, at(0), Press::Record(at(600))).unwrap();
    assert_eq!(
        intent_of(&conn, &Whose::House, at(0)).unwrap(),
        Some(Window::Off)
    );
    let state = capture::capture_state_of(&conn, &her, at(0)).unwrap();
    assert!(state.desired_running && state.running && state.settled);
}

#[test]
fn the_audit_names_the_verb_and_the_origin() {
    let conn = writable();
    record_control_origin(&conn, at(0), "record", "session:someone").unwrap();
    let (kind, detail): (String, String) = conn
        .query_row("SELECT kind, detail FROM capture_events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "control_request");
    assert_eq!(detail, "record — session:someone");
}

// --- the long-poll's own contract -------------------------------------------

#[test]
fn an_unchanged_state_fingerprints_the_same_twice() {
    // The token is a function of the state; varying per call, every hang
    // would return at once.
    let conn = writable();
    press(&conn, &Whose::House, at(0), Press::Record(at(1800))).unwrap();
    let a = fleet_capture_state(&conn, at(1)).unwrap();
    let b = fleet_capture_state(&conn, at(2)).unwrap();
    assert_eq!(a.state_token, b.state_token);
}

#[test]
fn a_press_changes_the_fingerprint_so_a_hanging_poll_wakes() {
    let conn = writable();
    let off = fleet_capture_state(&conn, at(0)).unwrap();
    press(&conn, &Whose::House, at(0), Press::Record(at(1800))).unwrap();
    let on = fleet_capture_state(&conn, at(0)).unwrap();
    assert_ne!(off.state_token, on.state_token);
}

#[test]
fn a_window_ending_changes_the_fingerprint_with_nobody_pressing_anything() {
    // Nothing writes when a window ends, so the handler re-derives each slice.
    let conn = writable();
    press(&conn, &Whose::House, at(0), Press::Record(at(1800))).unwrap();
    let during = fleet_capture_state(&conn, at(60)).unwrap();
    let after = fleet_capture_state(&conn, at(1801)).unwrap();
    assert_ne!(during.state_token, after.state_token);
    assert!(!after.desired_running, "an ended window is off");
}

// --- the audit descriptor ---------------------------------------------------

use recalld::webauth::request_origin;

fn gate() -> recalld::webauth::Config {
    recalld::webauth::Config {
        session_secret: "test-secret-not-a-real-one".into(),
        client_id: "cid".into(),
        client_secret: "csec".into(),
        nc_base_url: "https://dash.example.org".into(),
        nc_internal_url: "https://dash.example.org".into(),
        redirect_uri: "http://127.0.0.1/auth/callback".into(),
        allowed_users: std::collections::HashSet::new(),
        device_token: None,
    }
}

#[test]
fn without_a_sign_in_only_the_peer_is_known() {
    // The host still answers "was that press mine?" on a household network.
    assert_eq!(
        request_origin(
            &gate(),
            "POST",
            "/api/capture/pause",
            None,
            None,
            0,
            Some("10.0.0.5")
        ),
        "anon 10.0.0.5"
    );
}

#[test]
fn an_unknown_peer_is_named_rather_than_left_blank() {
    assert_eq!(
        request_origin(&gate(), "POST", "/api/capture/pause", None, None, 0, None),
        "anon unknown-host"
    );
}

use recalld::capture::client_host;

#[test]
fn behind_the_named_proxy_the_audit_names_the_real_client() {
    // isis's front door connects from the node for every caller; its header
    // says who.
    let proxy: std::net::IpAddr = "10.42.0.1".parse().unwrap();
    assert_eq!(
        client_host(Some(proxy), Some("10.100.0.12"), &[proxy]).as_deref(),
        Some("10.100.0.12")
    );
}

#[test]
fn a_forwarded_header_from_anyone_else_is_ignored() {
    // Any caller can send the header; only the named proxy is believed.
    let proxy: std::net::IpAddr = "10.42.0.1".parse().unwrap();
    let peer: std::net::IpAddr = "10.100.0.5".parse().unwrap();
    assert_eq!(
        client_host(Some(peer), Some("10.100.0.12"), &[proxy]).as_deref(),
        Some("10.100.0.5")
    );
}

#[test]
fn a_proxy_without_a_usable_header_is_still_named() {
    let proxy: std::net::IpAddr = "10.42.0.1".parse().unwrap();
    assert_eq!(
        client_host(Some(proxy), None, &[proxy]).as_deref(),
        Some("10.42.0.1")
    );
    assert_eq!(
        client_host(Some(proxy), Some("not an address"), &[proxy]).as_deref(),
        Some("10.42.0.1")
    );
}

#[test]
fn the_audit_write_cannot_refuse_the_control_action() {
    // A press must not depend on the audit: with no events table at all, the
    // intent is still recorded.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    let opened = press(&conn, &Whose::House, at(0), Press::Record(at(1800))).unwrap();
    assert!(record_control_origin(&conn, at(0), "record", "anon 10.0.0.5").is_err());
    assert_eq!(
        intent_of(&conn, &Whose::House, at(0)).unwrap(),
        Some(opened),
        "the window survived its own audit failing"
    );
}

// --- the routes are mounted, not merely written ------------------------------

/// Through the real router: a handler's tests say nothing about its mounting.
#[tokio::test]
async fn the_capture_routes_are_reachable_through_the_real_router() {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::app::prepare(&root).expect("planes");

    let app = recalld::app::router(std::sync::Arc::new(recalld::app::Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: recalld::app::DEFAULT_MAX_BODY,
        trusted_proxies: Vec::new(),
        // Without webauth the browsing plane, capture included, is absent.
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
            recorders: None,
        }),
        sync_token: None,
        frontend: None,
    }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // No session cookie: the mic apps long-poll this without a credential.
    let (status, body) =
        crate::http::request(&addr.to_string(), "GET", "/api/capture", &[], None).await;
    assert_eq!(status, 200, "the route is mounted AND ungated: {body}");

    // The shape of the capture contract, not just an answer.
    let state: serde_json::Value = serde_json::from_str(&body).expect("json");
    for field in [
        "running",
        "until",
        "held",
        "desiredRunning",
        "desiredUntil",
        "desiredHeld",
        "settled",
        "micReachable",
        "stateToken",
    ] {
        assert!(state.get(field).is_some(), "missing {field} in {state}");
    }
    assert_eq!(
        state["stateToken"].as_str().map(str::len),
        Some(12),
        "the fingerprint every client long-polls on"
    );
}

#[tokio::test]
async fn pressing_through_the_real_router_needs_no_login() {
    // The phones' buttons carry no credential.
    for (method, path) in [
        ("GET", "/api/capture"),
        ("POST", "/api/capture/record"),
        ("POST", "/api/capture/stop"),
        ("POST", "/api/capture/pause"),
        ("POST", "/api/capture/unpause"),
    ] {
        assert!(
            !recalld::webauth::requires_session(method, path),
            "{method} {path} must stay on the device-exempt plane"
        );
    }
}

// --- the notify, and the slice that must survive it -------------------------

/// A press reaches a waiting poll in about one round trip, not a 2 s slice.
#[tokio::test]
async fn a_press_wakes_a_waiting_poll_without_paying_a_slice() {
    let watcher = recalld::capture::intent_watch();
    let started = std::time::Instant::now();

    let waiter = tokio::spawn(async move {
        recalld::capture::wait_intent_changed(watcher, std::time::Duration::from_secs(5)).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    recalld::capture::notify_intent_changed();

    let woke = waiter.await.expect("waiter finished");
    assert!(woke, "the wait must report a change, not a timeout");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "woke after {:?}, which is a slice rather than a notify",
        started.elapsed()
    );
}

/// A change between deriving the state and starting the wait is not missed:
/// subscribing before the derive catches it.
#[tokio::test]
async fn a_change_landing_before_the_wait_starts_is_not_missed() {
    // A local channel: the real one is process-global, and another test's
    // press would let this pass for the wrong reason.
    let (tx, rx) = tokio::sync::watch::channel(0u64);

    // Subscribe first, then press where a real handler would be deriving state.
    let watcher = rx;
    tx.send_modify(|v| *v = v.wrapping_add(1));

    let started = std::time::Instant::now();
    let woke =
        recalld::capture::wait_intent_changed(watcher, std::time::Duration::from_secs(5)).await;

    assert!(
        woke,
        "a change before the wait must return at once, not time out"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(200),
        "waited {:?} for a change that had already happened",
        started.elapsed()
    );
}

/// The notify does not replace the slice: an expiring pause and a pause from
/// another process signal nothing, so the wait still times out.
#[tokio::test]
async fn a_transition_with_no_writer_still_surfaces_on_the_slice() {
    // A local channel, not `intent_watch()`: the timeout is under test, so no
    // other test may hold the sender.
    let (_tx, rx) = tokio::sync::watch::channel(0u64);
    let watcher = rx;
    let started = std::time::Instant::now();

    let woke =
        recalld::capture::wait_intent_changed(watcher, std::time::Duration::from_millis(150)).await;

    assert!(
        !woke,
        "nothing wrote, so this is a timeout and not a change"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(150),
        "returned early at {:?} — the slice is the floor",
        started.elapsed()
    );
}

/// The wire fixtures' tokens are what recalld stamps on their states.
#[test]
fn the_wire_fixtures_carry_the_token_recalld_stamps() {
    for name in ["capture-state-recording.json", "capture-state-off.json"] {
        let path = format!(
            "{}/../tests/fixtures/wire/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let recorded: CaptureState =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("fixture")).expect("reads");
        assert_eq!(capture::stamped(recorded.clone()), recorded, "{name}");
    }
}
