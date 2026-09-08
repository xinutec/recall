//! The capture-control state machine, and the fingerprint every client's
//! long-poll depends on.

use chrono::{DateTime, Utc};
use recalld::capture::{CaptureState, fleet_capture_state, intent_until, reported_state};
use rusqlite::Connection;

fn at(offset_s: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_788_894_682 + offset_s, 0).expect("a real instant")
}

/// A settings table, which is all this state lives in.
fn store(rows: &[(&str, &str)]) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
    for (k, v) in rows {
        conn.execute("INSERT INTO settings VALUES (?1, ?2)", [k, v])
            .unwrap();
    }
    conn
}

// Mirrors CaptureState's own field order; the bools are the contract.
#[allow(clippy::fn_params_excessive_bools)]
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
    // ⚠ NOT a value this implementation produced. On 2026-09-08 the running
    // household's `GET /api/capture` answered with exactly this token for
    // exactly this state. It pins the hash against PRODUCTION, so a change to
    // the field set, the key order or the JSON separators fails here rather
    // than silently turning every client's long-poll into a busy poll.
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
    // Computed from the Python (`hashlib.sha256(json.dumps(payload,
    // sort_keys=True))`) for the two other states a client actually sees.
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
    // The bounded-pause safety net: a pause that outlived its own deadline must
    // never keep a household silent because nobody cleared a row.
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
    // `settled` compares this string to what the Mac round-trips back, by
    // equality. Re-deriving it (say, normalising the offset) would make a
    // correctly-applied pause read as permanently transitioning.
    let conn = store(&[("capture_intent", "2026-09-08T19:11:22.164504+00:00")]);
    assert_eq!(
        intent_until(&conn, at(-1)).unwrap().as_deref(),
        Some("2026-09-08T19:11:22.164504+00:00")
    );
}

#[test]
fn a_stale_report_means_the_mic_is_not_reachable() {
    // Past the freshness window the Mac has stopped reporting, so intent is
    // shown as intent — never dressed up as confirmation.
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
    // This is what renders as "Pausing…" — desired has flipped, confirmed lags.
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
    // A snooze changes the resume-by while both sides agree on "paused". If
    // only `running` were compared, the new deadline would read as already
    // applied and the UI would claim a pause that has not taken effect.
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
    // resumed household reading as transitioning for ever.
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
    conn.execute_batch(
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE capture_events (
             id INTEGER PRIMARY KEY, utc TEXT NOT NULL, kind TEXT NOT NULL,
             source_id TEXT, detail TEXT);",
    )
    .unwrap();
    conn
}

#[test]
fn a_pause_is_always_bounded() {
    // The household's control is "stop recording", never "stop indefinitely".
    // A pause that outlives everyone's memory of setting it is how a week of
    // the archive goes missing without anyone deciding to lose it.
    let now = at(0);
    assert_eq!(
        compute_resume_by(now, None),
        now + chrono::Duration::hours(24)
    );
    // A request longer than the bound is CLAMPED, not honoured.
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
    // What was stored is what a reader gets back, spelled the same way — the
    // Mac compares this string by equality when it confirms.
    assert_eq!(
        intent_until(&conn, at(0)).unwrap().as_deref(),
        Some(iso.as_str())
    );

    intent_resume(&conn).unwrap();
    assert_eq!(intent_until(&conn, at(0)).unwrap(), None);
}

#[test]
fn a_resume_leaves_a_row_rather_than_deleting_it() {
    // A missing row and a cleared one mean the same thing to a reader, but the
    // mirror polls this value: keeping it means "resumed" is something it can
    // read, not an absence it has to infer.
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
    // End to end over the two halves: the press moves desired only, and the UI
    // shows "Pausing…" rather than claiming a pause that has not taken effect.
    let conn = writable();
    intent_pause(&conn, at(0), Some(30)).unwrap();
    let state = fleet_capture_state(&conn, at(0)).unwrap();
    assert!(!state.desired_running);
    assert!(!state.settled);
    // Nothing has been heard from the Mac at all, so say so.
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
    // ⚠ Regression guard. Formatting with a fixed precision wrote
    // `...22.000000+00:00` where Python writes `...22+00:00` — the same moment,
    // different TEXT. `settled` compares the intent to the Mac's echo by string
    // equality, so the mismatch would leave a correctly-applied pause reading as
    // transitioning for ever.
    let conn = writable();
    let iso = intent_pause(&conn, at(0), Some(30)).unwrap();
    assert!(!iso.contains(".000000"), "{iso}");
    assert!(iso.ends_with("+00:00"), "{iso}");
    // And a sub-second instant keeps its six digits, as Python does.
    let sub = DateTime::from_timestamp(1_788_894_682, 164_504_000).unwrap();
    let iso = intent_pause(&conn, sub, Some(0)).unwrap();
    assert!(iso.contains(".164504"), "{iso}");
}

// --- the long-poll's own contract -------------------------------------------

#[test]
fn an_unchanged_state_fingerprints_the_same_twice() {
    // The whole long-poll rests on this: the token is a pure function of the
    // state, so "unchanged" is the server's judgement and not a guess. If it
    // varied per call — a timestamp, a map iteration order — every hang would
    // return instantly and every recorder in the house would become a poller.
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
    // ⚠ The transition with NO notify behind it. Nothing writes when a pause
    // reaches its deadline, so a long-poll parked on a condition variable would
    // never wake — which is why the handler re-derives on a slice rather than
    // waiting to be told.
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
    // The Mac, dev, a LAN-only deployment: there is no plane, so claiming one
    // would be an invention. It still names the host, which is the whole
    // answer to "was that pause mine?" on a single-household network.
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
    // ⚠ The property, not the mechanism: silencing a household's microphone must
    // not depend on a bookkeeping write. Here the events table does not exist at
    // all, which is the harshest version of a failing audit — and the intent is
    // still recorded and readable afterwards.
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

/// ⚠ This test exists because of a real incident. On 2026-09-07 `/api/correct`
/// was written, tested, and its Python counterpart deleted in the same change —
/// and the route was never added to the router. Both suites were green and the
/// endpoint was served by NOBODY for a deploy. A handler with passing unit tests
/// says nothing about whether anything can reach it.
#[tokio::test]
async fn the_capture_routes_are_reachable_through_the_real_router() {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::store::open(&root).expect("ingest db");
    // The meaning plane the capture state lives in.
    let conn = recalld::work::open_write(&root).expect("recall db");
    conn.execute_batch(
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE capture_events (
             id INTEGER PRIMARY KEY, utc TEXT NOT NULL, kind TEXT NOT NULL,
             source_id TEXT, detail TEXT);",
    )
    .expect("schema");
    drop(conn);

    let app = recalld::app::router(std::sync::Arc::new(recalld::app::Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: recalld::app::DEFAULT_MAX_BODY,
        // ⚠ NOT None. `webauth: None` means the browsing plane is ABSENT, not
        // open — the deliberate inversion of this repo's inert-unless-configured
        // rule, so that an unconfigured recalld cannot serve household
        // transcripts to anyone who can reach the port. A test that passed None
        // would be asserting against a router with no capture routes in it.
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

    // NO session cookie, deliberately: GET /api/capture is on the device-exempt
    // plane because the mic apps long-poll it with no credential at all. If this
    // ever needs a login, every recorder in the house stops learning about pauses.
    let body = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("http://{addr}/api/capture"))
            .call()
            .expect("the route is mounted AND ungated")
            .into_string()
            .expect("a body")
    })
    .await
    .expect("task");

    // Not just "something answered": the SHAPE of the capture contract.
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
    // ⚠ The property that keeps the house controllable. The mic apps' pause and
    // resume buttons carry NO credential — login-free by choice — so if the gate
    // ever starts demanding a session here, every phone's pause button stops
    // working and the only way to silence the room is the web UI.
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
