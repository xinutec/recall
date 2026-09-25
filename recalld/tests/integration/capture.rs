//! The capture-control state machine, and the fingerprint every client's
//! long-poll depends on.

use chrono::{DateTime, Utc};
use recalld::capture::{CaptureState, fleet_capture_state, intent_until, reported_state};
use rusqlite::Connection;

fn at(offset_s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_788_894_682 + offset_s, 0).expect("a real instant")
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

// Mirrors CaptureState's own field order; the bools are the contract.
#[allow(
    clippy::fn_params_excessive_bools,
    reason = "mirrors CaptureState's field order (above)"
)]
fn state(
    running: bool,
    paused: Option<&str>,
    desired_running: bool,
    desired: Option<&str>,
    settled: bool,
    mic: bool,
) -> CaptureState {
    CaptureState {
        running,
        paused_until: paused.map(ToOwned::to_owned),
        desired_running,
        desired_paused_until: desired.map(ToOwned::to_owned),
        settled,
        mic_reachable: mic,
        state_token: String::new(),
    }
    .stamped()
}

#[test]
fn the_token_matches_the_one_the_live_fleet_served() {
    // The token production served for this state. A change to the field set,
    // key order or JSON separators fails here rather than silently turning
    // every client's long-poll into a busy poll.
    let live = state(
        false,
        Some("2026-09-08T19:11:22.164504+00:00"),
        false,
        Some("2026-09-08T19:11:22.164504+00:00"),
        true,
        true,
    );
    assert_eq!(live.state_token, "951046d8fc3c");
}

#[test]
fn the_token_matches_python_for_the_running_and_transitioning_shapes() {
    // `sha256(json.dumps(payload, sort_keys=True))` for the two other states a
    // client sees.
    let running = state(true, None, true, None, true, true);
    assert_eq!(running.state_token, "e9be2a3b3af3");

    let transitioning = state(
        true,
        None,
        false,
        Some("2026-09-08T19:11:22+00:00"),
        false,
        false,
    );
    assert_eq!(transitioning.state_token, "9f6805e7bc32");
}

#[test]
fn the_token_changes_when_any_graded_field_does() {
    let base = state(true, None, true, None, true, true);
    for other in [
        state(false, None, true, None, true, true),
        state(
            true,
            Some("2026-09-08T19:11:22+00:00"),
            true,
            None,
            true,
            true,
        ),
        state(true, None, false, None, true, true),
        state(
            true,
            None,
            true,
            Some("2026-09-08T19:11:22+00:00"),
            true,
            true,
        ),
        state(true, None, true, None, false, true),
        state(true, None, true, None, true, false),
    ] {
        assert_ne!(
            base.state_token, other.state_token,
            "a client would never see this change: {other:?}"
        );
    }
}

#[test]
fn no_intent_is_running() {
    let conn = store(&[]);
    assert_eq!(intent_until(&conn, at(0)).unwrap(), None);
    // A blank value is how a resume is recorded, and must read the same way.
    let conn = store(&[("capture_intent", "")]);
    assert_eq!(intent_until(&conn, at(0)).unwrap(), None);
}

#[test]
fn an_elapsed_intent_reads_as_running() {
    // A pause past its deadline must not keep the house silent because nobody
    // cleared the row.
    let conn = store(&[("capture_intent", "2026-09-08T19:11:22+00:00")]);
    assert_eq!(intent_until(&conn, at(1)).unwrap(), None);
    assert!(intent_until(&conn, at(-1)).unwrap().is_some());
}

#[test]
fn an_unparseable_intent_reads_as_running_rather_than_a_pause_nobody_can_clear() {
    let conn = store(&[("capture_intent", "soon")]);
    assert_eq!(intent_until(&conn, at(0)).unwrap(), None);
}

#[test]
fn the_intent_keeps_its_stored_spelling() {
    // `settled` compares this string to the Mac's echo by equality, so
    // normalising it would leave an applied pause transitioning for ever.
    let conn = store(&[("capture_intent", "2026-09-08T19:11:22.164504+00:00")]);
    assert_eq!(
        intent_until(&conn, at(-1)).unwrap().as_deref(),
        Some("2026-09-08T19:11:22.164504+00:00")
    );
}

#[test]
fn a_stale_report_means_the_mic_is_not_reachable() {
    // Past the 30 s freshness window, intent is shown as intent, never as
    // confirmation.
    let conn = store(&[
        ("capture_reported_running", "1"),
        ("capture_reported_at", "2026-09-08T19:11:22+00:00"),
    ]);
    assert!(reported_state(&conn, at(29)).unwrap().is_some());
    assert!(reported_state(&conn, at(31)).unwrap().is_none());

    let state = fleet_capture_state(&conn, at(31)).unwrap();
    assert!(!state.mic_reachable);
    assert!(!state.settled);
}

#[test]
fn a_confirmed_pause_is_settled() {
    let when = "2026-09-08T19:11:32+00:00";
    let conn = store(&[
        ("capture_intent", when),
        ("capture_reported_running", "0"),
        ("capture_reported_paused_until", when),
        ("capture_reported_at", "2026-09-08T19:11:22+00:00"),
    ]);
    let state = fleet_capture_state(&conn, at(1)).unwrap();
    assert!(!state.running);
    assert!(!state.desired_running);
    assert!(state.settled);
    assert!(state.mic_reachable);
}

#[test]
fn a_pause_the_mac_has_not_applied_yet_is_unsettled() {
    // Renders as "Pausing…": desired has flipped, confirmed lags.
    let conn = store(&[
        ("capture_intent", "2026-09-08T19:11:32+00:00"),
        ("capture_reported_running", "1"),
        ("capture_reported_at", "2026-09-08T19:11:22+00:00"),
    ]);
    let state = fleet_capture_state(&conn, at(1)).unwrap();
    assert!(
        state.running,
        "the mic's confirmed word is still 'recording'"
    );
    assert!(!state.desired_running);
    assert!(!state.settled);
}

#[test]
fn extending_a_pause_reads_as_transitioning_until_applied() {
    // A snooze changes the resume-by while both sides agree on "paused";
    // comparing only `running` would claim the new deadline as applied.
    let conn = store(&[
        ("capture_intent", "2026-09-08T20:11:22+00:00"),
        ("capture_reported_running", "0"),
        ("capture_reported_paused_until", "2026-09-08T19:11:32+00:00"),
        ("capture_reported_at", "2026-09-08T19:11:22+00:00"),
    ]);
    let state = fleet_capture_state(&conn, at(1)).unwrap();
    assert!(!state.settled, "the Mac is still on the OLD resume-by");
    assert_eq!(
        state.paused_until.as_deref(),
        Some("2026-09-08T19:11:32+00:00")
    );
    assert_eq!(
        state.desired_paused_until.as_deref(),
        Some("2026-09-08T20:11:22+00:00")
    );
}

#[test]
fn a_confirmed_resume_is_settled_without_comparing_deadlines() {
    // Running has no resume-by to agree about; requiring one would leave a
    // resume transitioning for ever.
    let conn = store(&[
        ("capture_intent", ""),
        ("capture_reported_running", "1"),
        ("capture_reported_paused_until", "2026-09-08T19:11:32+00:00"),
        ("capture_reported_at", "2026-09-08T19:11:22+00:00"),
    ]);
    let state = fleet_capture_state(&conn, at(1)).unwrap();
    assert!(state.settled);
    assert!(state.running);
}

// --- the write half: intent, and the audit of who asked ---------------------

use recalld::capture::{compute_resume_by, intent_pause, intent_resume, record_control_origin};

fn writable() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

#[test]
fn a_pause_is_always_bounded() {
    // A pause is never indefinite: a forgotten one would lose days of the
    // archive without anyone deciding to.
    let now = at(0);
    assert_eq!(
        compute_resume_by(now, None),
        now + chrono::Duration::hours(24)
    );
    // A request longer than 24 h is clamped.
    assert_eq!(
        compute_resume_by(now, Some(60 * 48)),
        now + chrono::Duration::hours(24)
    );
    assert_eq!(
        compute_resume_by(now, Some(30)),
        now + chrono::Duration::minutes(30)
    );
}

#[test]
fn a_negative_pause_clamps_to_now_rather_than_minting_an_elapsed_one() {
    let now = at(0);
    assert_eq!(compute_resume_by(now, Some(-5)), now);
}

#[test]
fn pausing_then_resuming_round_trips_through_the_settings_row() {
    let conn = writable();
    let iso = intent_pause(&conn, at(0), Some(30)).unwrap();
    // A reader gets the stored spelling back: the Mac's confirmation compares
    // it by equality.
    assert_eq!(
        intent_until(&conn, at(0)).unwrap().as_deref(),
        Some(iso.as_str())
    );

    intent_resume(&conn).unwrap();
    assert_eq!(intent_until(&conn, at(0)).unwrap(), None);
}

#[test]
fn a_resume_leaves_a_row_rather_than_deleting_it() {
    // The mirror polls this value, so "resumed" is a value it reads rather than
    // an absence it infers.
    let conn = writable();
    intent_pause(&conn, at(0), Some(30)).unwrap();
    intent_resume(&conn).unwrap();
    let raw: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key='capture_intent'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(raw, "");
}

#[test]
fn a_fresh_pause_reads_as_unsettled_until_the_mac_confirms() {
    // The press moves desired only, so the UI shows "Pausing…" rather than
    // claiming a pause that has not taken effect.
    let conn = writable();
    intent_pause(&conn, at(0), Some(30)).unwrap();
    let state = fleet_capture_state(&conn, at(0)).unwrap();
    assert!(!state.desired_running);
    assert!(!state.settled);
    // Nothing has been heard from the Mac.
    assert!(!state.mic_reachable);
}

#[test]
fn the_audit_names_the_verb_and_the_origin() {
    let conn = writable();
    record_control_origin(&conn, at(0), "pause", "session:someone").unwrap();
    let (kind, detail): (String, String) = conn
        .query_row("SELECT kind, detail FROM capture_events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(kind, "control_request");
    assert_eq!(detail, "pause — session:someone");
}

#[test]
fn a_whole_second_pause_is_spelled_without_a_fraction() {
    // `isoformat()` spelling: `...22+00:00`, not `...22.000000+00:00`. The
    // intent is compared to other spellings of it as text, so a fixed precision
    // would leave an applied pause transitioning for ever.
    let conn = writable();
    let iso = intent_pause(&conn, at(0), Some(30)).unwrap();
    assert!(!iso.contains(".000000"), "{iso}");
    assert!(iso.ends_with("+00:00"), "{iso}");
    // A sub-second instant keeps its six digits.
    let sub = DateTime::from_timestamp(1_788_894_682, 164_504_000).unwrap();
    let iso = intent_pause(&conn, sub, Some(0)).unwrap();
    assert!(iso.contains(".164504"), "{iso}");
}

// --- the long-poll's own contract -------------------------------------------

#[test]
fn an_unchanged_state_fingerprints_the_same_twice() {
    // The token is a pure function of the state. If it varied per call, every
    // hang would return at once and every recorder would become a busy poller.
    let conn = writable();
    intent_pause(&conn, at(0), Some(30)).unwrap();
    let a = fleet_capture_state(&conn, at(1)).unwrap();
    let b = fleet_capture_state(&conn, at(2)).unwrap();
    assert_eq!(a.state_token, b.state_token);
}

#[test]
fn a_press_changes_the_fingerprint_so_a_hanging_poll_wakes() {
    let conn = writable();
    intent_resume(&conn).unwrap();
    let running = fleet_capture_state(&conn, at(0)).unwrap();
    intent_pause(&conn, at(0), Some(30)).unwrap();
    let paused = fleet_capture_state(&conn, at(0)).unwrap();
    assert_ne!(running.state_token, paused.state_token);
}

#[test]
fn a_pause_elapsing_changes_the_fingerprint_with_nobody_pressing_anything() {
    // Nothing writes when a pause reaches its deadline, so no notify fires;
    // this is why the handler re-derives on a slice.
    let conn = writable();
    intent_pause(&conn, at(0), Some(30)).unwrap();
    let during = fleet_capture_state(&conn, at(60)).unwrap();
    let after = fleet_capture_state(&conn, at(60 * 31)).unwrap();
    assert_ne!(during.state_token, after.state_token);
    assert!(after.desired_running, "an elapsed pause reads as running");
}

// --- the audit descriptor ---------------------------------------------------

use recalld::webauth::request_origin;

#[test]
fn with_no_gate_configured_only_the_peer_is_known() {
    // No plane is claimed where there is none; the host still answers "was
    // that pause mine?" on a household network.
    assert_eq!(
        request_origin(
            None,
            "POST",
            "/api/capture/pause",
            None,
            None,
            0,
            Some("10.0.0.5")
        ),
        "no-auth 10.0.0.5"
    );
}

#[test]
fn an_unknown_peer_is_named_rather_than_left_blank() {
    assert_eq!(
        request_origin(None, "POST", "/api/capture/pause", None, None, 0, None),
        "no-auth unknown-host"
    );
}

#[test]
fn the_audit_write_cannot_refuse_the_control_action() {
    // Silencing the microphone must not depend on a bookkeeping write. The
    // events table is missing entirely, and the intent is still recorded.
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    let iso = intent_pause(&conn, at(0), Some(30)).unwrap();
    assert!(record_control_origin(&conn, at(0), "pause", "anon 10.0.0.5").is_err());
    assert_eq!(
        intent_until(&conn, at(0)).unwrap().as_deref(),
        Some(iso.as_str()),
        "the pause survived its own audit failing"
    );
}

// --- the routes are MOUNTED, not merely written ------------------------------

/// Through the real router, because a handler's passing unit tests say nothing
/// about whether it is mounted.
#[tokio::test]
async fn the_capture_routes_are_reachable_through_the_real_router() {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::store::open(&root).expect("ingest db");
    let conn = recalld::work::open_write(&root).expect("recall db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    drop(conn);

    let app = recalld::app::router(std::sync::Arc::new(recalld::app::Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: recalld::app::DEFAULT_MAX_BODY,
        // ⚠ Not None: without webauth the browsing plane, capture routes
        // included, is absent rather than open.
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
        frontend: None,
    }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // No session cookie: the mic apps long-poll GET /api/capture with no
    // credential, so it must stay device-exempt.
    let body = tokio::task::spawn_blocking(move || {
        // Not `ureq::get`: its process-wide pool is shared by parallel tests
        // against short-lived servers.
        ureq::AgentBuilder::new()
            .max_idle_connections(0)
            .build()
            .get(&format!("http://{addr}/api/capture"))
            .call()
            .expect("the route is mounted AND ungated")
            .into_string()
            .expect("a body")
    })
    .await
    .expect("task");

    // The shape of the capture contract, not just an answer.
    let state: serde_json::Value = serde_json::from_str(&body).expect("json");
    for field in [
        "running",
        "pausedUntil",
        "desiredRunning",
        "desiredPausedUntil",
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
async fn pausing_through_the_real_router_needs_no_login() {
    // The mic apps' pause and resume buttons carry no credential by choice; a
    // session requirement here would break every phone's pause button.
    for (method, path) in [
        ("GET", "/api/capture"),
        ("POST", "/api/capture/pause"),
        ("POST", "/api/capture/resume"),
    ] {
        assert!(
            !recalld::webauth::requires_session(method, path),
            "{method} {path} must stay on the device-exempt plane"
        );
    }
}

// --- the notify, and the slice that must survive it -------------------------

/// A press reaches a waiting poll in about one round trip, not a 2 s slice: the
/// `/api/capture` writers and readers share this process, so a notify reaches
/// them.
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

/// ⚠ The lost wakeup: a change landing between deriving the state and starting
/// the wait must not be missed. Subscribing before the derive closes it.
#[tokio::test]
async fn a_change_landing_before_the_wait_starts_is_not_missed() {
    // ⚠ A local channel: the real signal is process-global, and a neighbouring
    // test's press would let this pass for the wrong reason.
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

/// ⚠ The notify must not replace the slice. A pause elapsing and a CLI pause from
/// another process have no writer that can signal, so the wait still times out
/// and lets the caller re-derive.
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
