//! The Mac→fleet sync plane: its gate, the capture handshake, and the routes
//! the Mac's agents read.

use recalld::capture::{intent_pause, record_reported, reported_state};
use recalld::sync::{IntentOut, bearer, check};
use rusqlite::Connection;

/// A one-shot HTTP agent with no connection pool.
///
fn at(offset_s: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_788_894_682 + offset_s, 0).expect("a real instant")
}

fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn
}

fn setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

fn liveness(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
        .collect()
}

// --- the gate ----------------------------------------------------------------

#[test]
fn only_a_bearer_header_carries_a_token() {
    assert_eq!(bearer(Some("Bearer sekrit")), Some("sekrit"));
    assert_eq!(bearer(None), None);
    // Not a bearer scheme: reading the tail as a token would accept a Basic
    // credential as a sync one.
    assert_eq!(bearer(Some("sekrit")), None);
    assert_eq!(bearer(Some("Basic sekrit")), None);
    assert_eq!(bearer(Some("bearer sekrit")), None);
}

#[test]
fn a_missing_or_wrong_token_is_rejected() {
    assert!(check(Some("sekrit"), "sekrit").is_ok());
    assert!(check(None, "sekrit").is_err());
    assert!(check(Some(""), "sekrit").is_err());
    assert!(check(Some("sekri"), "sekrit").is_err());
    assert!(check(Some("sekritt"), "sekrit").is_err());
    // Nor may a string that merely starts with the secret.
    assert!(check(Some("sekrit-and-more"), "sekrit").is_err());
}

// --- what the report writes --------------------------------------------------

#[test]
fn a_reported_pause_is_stored_verbatim_so_settled_can_compare_it() {
    // Stored verbatim: `settled` compares the Mac's echo as a string.
    let conn = store();
    let applied = "2026-09-09T21:15:05.645698+00:00";

    record_reported(&conn, at(0), false, Some(applied), &liveness(&[])).unwrap();

    assert_eq!(
        setting(&conn, "capture_reported_paused_until").as_deref(),
        Some(applied)
    );
    let back = reported_state(&conn, at(1)).unwrap().expect("fresh");
    assert_eq!(back.paused_until.as_deref(), Some(applied));
    assert!(!back.running);
}

#[test]
fn the_intent_and_the_echo_of_it_settle() {
    let conn = store();
    let intent = intent_pause(&conn, at(0), Some(30)).unwrap();

    record_reported(&conn, at(5), false, Some(&intent), &liveness(&[])).unwrap();

    let state = recalld::capture::fleet_capture_state(&conn, at(6)).unwrap();
    assert!(state.settled, "the echo of an intent must settle it");
    assert_eq!(state.paused_until.as_deref(), Some(intent.as_str()));
}

#[test]
fn recording_stores_an_empty_pause_not_a_missing_row() {
    // Written, so "reported as running" is not "never reported".
    let conn = store();

    record_reported(&conn, at(0), true, None, &liveness(&[])).unwrap();

    assert_eq!(
        setting(&conn, "capture_reported_paused_until").as_deref(),
        Some("")
    );
    let back = reported_state(&conn, at(1)).unwrap().expect("fresh");
    assert!(back.running);
    assert_eq!(back.paused_until, None);
}

#[test]
fn the_report_stamps_the_freshness_clock_python_spells_it_with() {
    // Every reader gates on this key's age.
    let conn = store();

    record_reported(&conn, at(0), true, None, &liveness(&[])).unwrap();

    assert_eq!(
        setting(&conn, "capture_reported_at").as_deref(),
        // Python's `isoformat()` spelling: no microseconds when they are zero,
        // so a fixed-precision `.000000` fails here.
        Some("2026-09-08T19:11:22+00:00")
    );
    // Freshness is measured from it: the window is 30 s.
    assert!(reported_state(&conn, at(29)).unwrap().is_some());
    assert!(reported_state(&conn, at(31)).unwrap().is_none());
}

#[test]
fn source_liveness_is_stored_as_python_would_dump_it() {
    // `json.dumps` separators and the Mac's key order (`preserve_order`).
    let conn = store();

    record_reported(
        &conn,
        at(0),
        true,
        None,
        &liveness(&[
            ("usb", "2026-09-08T02:31:00+00:00"),
            ("pixel5", "2026-09-08T02:30:00+00:00"),
        ]),
    )
    .unwrap();

    assert_eq!(
        setting(&conn, "capture_reported_source_liveness").as_deref(),
        Some(r#"{"usb": "2026-09-08T02:31:00+00:00", "pixel5": "2026-09-08T02:30:00+00:00"}"#),
        "separators AND the Mac's key order, not serde's alphabetical one"
    );
}

#[test]
fn an_absent_liveness_map_stores_an_empty_object() {
    // An older Mac without the field reports no liveness.
    let conn = store();

    record_reported(&conn, at(0), true, None, &liveness(&[])).unwrap();

    assert_eq!(
        setting(&conn, "capture_reported_source_liveness").as_deref(),
        Some("{}")
    );
}

// --- the wire shape ----------------------------------------------------------

#[test]
fn the_reply_is_camel_case_and_null_when_running() {
    // `pausedUntil`, with an explicit null.
    assert_eq!(
        serde_json::to_string(&IntentOut { paused_until: None }).unwrap(),
        r#"{"pausedUntil":null}"#
    );
    assert_eq!(
        serde_json::to_string(&IntentOut {
            paused_until: Some("2026-09-09T21:15:05.645698+00:00".into()),
        })
        .unwrap(),
        r#"{"pausedUntil":"2026-09-09T21:15:05.645698+00:00"}"#
    );
}

// --- the route is mounted, not merely written --------------------------------

/// A served router on the real schema, with the sync token set or not.
async fn serve(token: Option<&str>) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::store::open(&root).expect("ingest db");
    // The real migration ladder.
    let conn = recalld::work::open_write(&root).expect("recall db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    drop(conn);

    let app = recalld::app::router(std::sync::Arc::new(recalld::app::Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: recalld::app::DEFAULT_MAX_BODY,
        trusted_proxies: Vec::new(),
        // Absent, to show the sync plane does not need the SSO gate.
        webauth: None,
        sync_token: token.map(ToOwned::to_owned),
        frontend: None,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (dir, addr)
}

/// `POST /sync/capture` with a bearer token, returning (status, body).
async fn post(addr: &str, token: Option<&str>, body: serde_json::Value) -> (u16, String) {
    let bearer = token.map(|t| format!("Bearer {t}"));
    let mut headers = vec![("Content-Type", "application/json")];
    if let Some(bearer) = &bearer {
        headers.push(("Authorization", bearer));
    }
    crate::http::request(
        addr,
        "POST",
        "/sync/capture",
        &headers,
        Some(&body.to_string()),
    )
    .await
}

/// Over HTTP, to show the route is mounted.
#[tokio::test]
async fn the_capture_handshake_is_reachable_and_returns_the_fleets_intent() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    let intent = intent_pause(&conn, chrono::Utc::now(), Some(30)).unwrap();
    drop(conn);

    let (status, body) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({"running": true, "pausedUntil": null}),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, format!(r#"{{"pausedUntil":"{intent}"}}"#));
}

#[tokio::test]
async fn the_report_lands_where_api_capture_reads_it() {
    // The report reaches the state the UI reads.
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    let intent = intent_pause(&conn, chrono::Utc::now(), Some(30)).unwrap();
    drop(conn);

    let (status, _) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({
            "running": false,
            "pausedUntil": intent,
            "sourceLiveness": {"usb": "2026-09-08T19:11:00+00:00"},
        }),
    )
    .await;
    assert_eq!(status, 200);

    let conn = recalld::work::open_write(dir.path()).expect("db");
    let state = recalld::capture::fleet_capture_state(&conn, chrono::Utc::now()).unwrap();
    assert!(state.settled, "the Mac's echo settles the pause");
    assert!(state.mic_reachable, "a fresh report means the mic answered");
    assert!(!state.running);
    assert_eq!(
        setting(&conn, "capture_reported_source_liveness").as_deref(),
        Some(r#"{"usb": "2026-09-08T19:11:00+00:00"}"#)
    );
}

#[tokio::test]
async fn an_unauthenticated_report_cannot_move_the_state() {
    // And nothing was written.
    let (dir, addr) = serve(Some("sekrit")).await;

    for token in [None, Some("wrong")] {
        let (status, _) = post(
            &addr,
            token,
            serde_json::json!({"running": true, "pausedUntil": null}),
        )
        .await;
        assert_eq!(status, 401, "token {token:?}");
    }

    let conn = recalld::work::open_write(dir.path()).expect("db");
    assert_eq!(setting(&conn, "capture_reported_at"), None);
    assert!(
        recalld::capture::reported_state(&conn, chrono::Utc::now())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn without_a_configured_token_the_route_is_absent_not_open() {
    // An unconfigured token does not mean "open": the route is not mounted.
    let (_dir, addr) = serve(None).await;

    let (status, _) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({"running": true, "pausedUntil": null}),
    )
    .await;

    assert_eq!(status, 404, "an unconfigured sync plane must not answer");
}

#[tokio::test]
async fn a_non_string_liveness_value_is_refused_like_pydantic_refuses_it() {
    let (_dir, addr) = serve(Some("sekrit")).await;

    let (status, _) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({
            "running": true, "pausedUntil": null,
            "sourceLiveness": {"usb": 5},
        }),
    )
    .await;

    assert_eq!(status, 422);
}

#[tokio::test]
async fn a_long_poll_returns_at_once_when_the_intent_already_differs() {
    // A stale `knownIntent` is answered at once.
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    let intent = intent_pause(&conn, chrono::Utc::now(), Some(30)).unwrap();
    drop(conn);

    let started = std::time::Instant::now();
    let (status, body) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({
            "running": true, "pausedUntil": null,
            "wait": 25, "knownIntent": null,
        }),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, format!(r#"{{"pausedUntil":"{intent}"}}"#));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "held for {:?} despite a changed intent",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_long_poll_hangs_while_the_intent_is_unchanged_and_wakes_on_a_pause() {
    // The fleet cannot dial the Mac: a pause reaches it through the waiting
    // request.
    let (dir, addr) = serve(Some("sekrit")).await;
    let root = dir.path().to_path_buf();

    let hang = tokio::spawn({
        let addr = addr.clone();
        async move {
            post(
                &addr,
                Some("sekrit"),
                serde_json::json!({
                    "running": true, "pausedUntil": null,
                    "wait": 20, "knownIntent": null,
                }),
            )
            .await
        }
    });

    // Let it get into the hang, then press pause the way the UI would.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let conn = recalld::work::open_write(&root).expect("db");
    let intent = intent_pause(&conn, chrono::Utc::now(), Some(30)).unwrap();
    drop(conn);

    let started = std::time::Instant::now();
    let (status, body) = hang.await.expect("the hang finished");

    assert_eq!(status, 200);
    assert_eq!(
        body,
        format!(r#"{{"pausedUntil":"{intent}"}}"#),
        "the hang must return the NEW intent, not the one it started with"
    );
    // 500 ms separates a notify wake (about one round trip) from waiting out the
    // 2 s poll slice.
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "woke {:?} after the press — that is a slice, not a notify",
        started.elapsed()
    );
}

// --- lock contention ---------------------------------------------------------

/// Lock contention delays a handshake rather than failing it: the lock is held
/// 7 s, past a 5 s busy timeout.
#[tokio::test]
async fn a_writer_holding_the_lock_delays_the_handshake_rather_than_failing_it() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let root = dir.path().to_path_buf();

    // A second writer takes the lock and keeps it past 5 s.
    let held = std::time::Duration::from_secs(7);
    let holder = tokio::task::spawn_blocking({
        let root = root.clone();
        move || {
            let conn = recalld::work::open_write(&root).expect("db");
            conn.execute_batch("BEGIN IMMEDIATE; INSERT INTO settings VALUES ('held','1')")
                .expect("take the lock");
            std::thread::sleep(held);
            conn.execute_batch("COMMIT").expect("release");
        }
    });
    // Let the holder actually acquire it before the handshake starts.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let started = std::time::Instant::now();
    let (status, _) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({"running": true, "pausedUntil": null}),
    )
    .await;
    let waited = started.elapsed();
    holder.await.expect("holder finished");

    assert_eq!(
        status, 200,
        "contention must delay the report, never drop it"
    );
    assert!(
        waited > std::time::Duration::from_secs(5),
        "it answered in {waited:?} — it cannot have waited out a lock held for {held:?}, \
         so this test is no longer exercising contention"
    );
}

// --- the read routes ---------------------------------------------------------

/// `GET` one of the sync plane's read routes.
async fn get(addr: &str, path: &str, token: Option<&str>) -> (u16, String) {
    let bearer = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = bearer
        .iter()
        .map(|b| ("Authorization", b.as_str()))
        .collect();
    crate::http::request(addr, "GET", path, &headers, None).await
}

/// The read routes are mounted and gated: they carry the household's names.
#[tokio::test]
async fn every_sync_read_route_is_mounted_and_gated() {
    // With tables missing these answer 200 and empty; hence the real ladder.
    let (_dir, addr) = serve(Some("sekrit")).await;

    for path in [
        "/sync/vocabulary/prompt",
        "/sync/heard?since=2026-09-23T10:00:00%2B00:00&until=2026-09-23T10:30:00%2B00:00",
        "/sync/devices/heartbeats",
        "/sync/devices/outbox",
    ] {
        let (ok, body) = get(&addr, path, Some("sekrit")).await;
        assert_eq!(ok, 200, "{path} must be MOUNTED: {body}");

        for bad in [None, Some("wrong")] {
            let (refused, _) = get(&addr, path, bad).await;
            assert_eq!(refused, 401, "{path} must be GATED (token {bad:?})");
        }
    }
}

/// Fleetwatch's mic and upload checks read these with the sync token.
#[tokio::test]
async fn the_mac_reads_each_recorders_last_beat_and_outbox() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    recalld::devices::record_beat(
        &conn,
        &recalld::devices::Beat {
            device: "pixel5".into(),
            app: "android".into(),
            version: "0.11 (11)".into(),
            started_at: None,
            streaming: true,
            charging: None,
            mic_ok: Some(true),
            via_lan: None,
            dropped_bytes: Some(96_000),
            at: "2026-09-23T10:00:00+00:00".into(),
        },
        at(0),
    )
    .expect("beat");
    recalld::devices::record_report(
        &conn,
        &recalld::devices::Report {
            device: "pixel5".into(),
            queued: 2,
            oldest_queued_at: None,
            failing: 0,
            reason: None,
            at: "2026-09-23T10:00:00+00:00".into(),
        },
        at(0),
    )
    .expect("report");
    drop(conn);

    let (status, body) = get(&addr, "/sync/devices/heartbeats", Some("sekrit")).await;
    assert_eq!(status, 200, "{body}");
    let beats: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(beats["items"][0]["device"], "pixel5");
    assert_eq!(beats["items"][0]["micOk"], true);
    assert_eq!(beats["items"][0]["droppedBytes"], 96_000);
    assert_eq!(beats["items"][0]["at"], "2026-09-23T10:00:00+00:00");

    let (status, body) = get(&addr, "/sync/devices/outbox", Some("sekrit")).await;
    assert_eq!(status, 200, "{body}");
    let reports: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(reports["items"][0]["queued"], 2);
}

/// Without a token the read routes are absent, not open.
#[tokio::test]
async fn the_read_routes_are_absent_when_no_token_is_configured() {
    let (_dir, addr) = serve(None).await;

    for path in [
        "/sync/vocabulary/prompt",
        "/sync/live/health",
        "/sync/heard",
        "/sync/devices/heartbeats",
        "/sync/devices/outbox",
    ] {
        let (status, _) = get(&addr, path, Some("sekrit")).await;
        assert_eq!(status, 404, "{path} must not answer at all");
    }
}

/// Nothing changes: the route answers when the caller's wait runs out, not at
/// the 25 s cap.
#[tokio::test]
async fn a_wait_that_elapses_with_no_change_returns_the_intent_it_started_with() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    let intent = intent_pause(&conn, chrono::Utc::now(), Some(30)).unwrap();
    drop(conn);

    // `knownIntent` equals what is stored, so the route hangs until the wait
    // runs out.
    let started = std::time::Instant::now();
    let (status, body) = post(
        &addr,
        Some("sekrit"),
        serde_json::json!({
            "running": false, "pausedUntil": intent,
            "wait": 1, "knownIntent": intent,
        }),
    )
    .await;
    let held = started.elapsed();

    assert_eq!(status, 200);
    assert_eq!(
        body,
        format!(r#"{{"pausedUntil":"{intent}"}}"#),
        "the unchanged intent must still come back"
    );
    assert!(
        held >= std::time::Duration::from_millis(500),
        "returned in {held:?} — it did not wait at all, so the hang is not real"
    );
    assert!(
        held < std::time::Duration::from_secs(10),
        "held for {held:?} — the wait is not bounded by what the caller asked for"
    );
}

// --- the live tier's own numbers --------------------------------------------

/// `GET /sync/live/health` with the stamps spelled the way a caller sends them.
async fn live_health(addr: &str, token: Option<&str>, since: &str) -> (u16, String) {
    let since = crate::http::query(since);
    let until = crate::http::query("2026-09-21T12:00:00+00:00");
    let path =
        format!("/sync/live/health?lag_since={since}&window_since={since}&window_until={until}");
    get(addr, &path, token).await
}

#[tokio::test]
async fn the_live_tier_numbers_are_reachable_and_gated() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    conn.execute_batch(
        "INSERT INTO transcript_segments
             (asr_model, text, start_utc, end_utc, created_utc)
         VALUES ('live', 'ja dat doen we', '2026-09-21T11:41:00+00:00',
                 '2026-09-21T11:41:30+00:00', '2026-09-21T11:41:34+00:00');",
    )
    .expect("turn");
    drop(conn);

    for token in [None, Some("wrong")] {
        let (status, _) = live_health(&addr, token, "2026-09-21T11:40:00+00:00").await;
        assert_eq!(status, 401, "the live numbers are not open");
    }

    let (status, body) = live_health(&addr, Some("sekrit"), "2026-09-21T11:40:00+00:00").await;
    assert_eq!(status, 200);
    assert!(body.contains(r#""lagSamples":1"#), "{body}");
    assert!(body.contains(r#""lagMedianS":4.0"#), "{body}");
}

#[tokio::test]
async fn the_same_moment_spelled_two_ways_gives_the_same_answer() {
    // Timestamps compare as text and `Z` sorts after `+`: the server re-spells
    // the bound, or the first row would be dropped.
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    conn.execute_batch(
        "INSERT INTO transcript_segments
             (asr_model, text, start_utc, end_utc, created_utc)
         VALUES ('live', 'ja', '2026-09-21T11:40:00+00:00',
                 '2026-09-21T11:40:00+00:00', '2026-09-21T11:40:04+00:00');",
    )
    .expect("turn");
    drop(conn);

    // The turn ends exactly on the boundary, the only place the two spellings
    // disagree.
    for spelling in ["2026-09-21T11:40:00+00:00", "2026-09-21T11:40:00Z"] {
        let (status, body) = live_health(&addr, Some("sekrit"), spelling).await;
        assert_eq!(status, 200, "{spelling}: {body}");
        assert!(body.contains(r#""lagSamples":1"#), "{spelling}: {body}");
    }
}

#[tokio::test]
async fn a_window_bound_that_is_not_an_instant_is_refused() {
    // A 400.
    let (_dir, addr) = serve(Some("sekrit")).await;
    let (status, _) = live_health(&addr, Some("sekrit"), "yesterday").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn the_record_health_answers_the_doctor_and_nobody_else() {
    let (_dir, addr) = serve(Some("sekrit")).await;
    let path = "/sync/record/health?since=2026-09-28T00:00:00Z";

    let (status, body) = get(&addr, path, Some("sekrit")).await;
    let (refused, _) = get(&addr, path, Some("wrong")).await;

    assert_eq!(status, 200, "{body}");
    let health: audiocore::record_health::RecordHealth =
        serde_json::from_str(&body).expect("the doctor's shape");
    assert_eq!(health, audiocore::record_health::RecordHealth::default());
    assert_eq!(refused, 401);
}
