//! The Mac→fleet sync plane: its gate, and the capture handshake's parity with
//! the Python it replaces.

use recalld::capture::{intent_pause, record_reported, reported_state};
use recalld::sync::{IntentOut, bearer, check};
use rusqlite::Connection;

fn at(offset_s: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(1_788_894_682 + offset_s, 0).expect("a real instant")
}

fn store() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .unwrap();
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
    // Not a bearer scheme: Python's `startswith` rejects these too, and reading
    // the tail of one as a token would accept a Basic credential as a sync one.
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
    // ⚠ A prefix of the secret must not pass. The compare is over the whole
    // string, so this is really asserting there is no `starts_with` in there.
    assert!(check(Some("sekrit-and-more"), "sekrit").is_err());
}

// --- what the report writes --------------------------------------------------

#[test]
fn a_reported_pause_is_stored_verbatim_so_settled_can_compare_it() {
    // ⚠ THE property this whole exchange turns on. The Mac echoes back the exact
    // intent string it read, and `fleet_capture_state` decides `settled` by
    // STRING equality — so a spelling this end normalises is a pause that reads
    // as forever-pending, and the UI never stops saying "Pausing…".
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
    // End to end on the strings alone: pause, hand the Mac what it reads, take
    // its echo back, and the two must be equal.
    let conn = store();
    let intent = intent_pause(&conn, at(0), Some(30)).unwrap();

    record_reported(&conn, at(5), false, Some(&intent), &liveness(&[])).unwrap();

    let state = recalld::capture::fleet_capture_state(&conn, at(6)).unwrap();
    assert!(state.settled, "the echo of an intent must settle it");
    assert_eq!(state.paused_until.as_deref(), Some(intent.as_str()));
}

#[test]
fn recording_stores_an_empty_pause_not_a_missing_row() {
    // Python writes `paused_until or ""`. An absent row and an empty one already
    // mean the same thing to the reader; writing the row means a reader never
    // has to tell "never reported" from "reported as running".
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
    // Every reader of the reported state gates on this key's age, so a report
    // that lands without it reads as a Mac that has stopped checking in. The
    // spelling matters because the Python tier parses the same row.
    let conn = store();

    record_reported(&conn, at(0), true, None, &liveness(&[])).unwrap();

    assert_eq!(
        setting(&conn, "capture_reported_at").as_deref(),
        // ⚠ Not a value this implementation produced: it is what CPython printed
        // for `datetime.fromtimestamp(1788894682, timezone.utc).isoformat()`.
        // Note the absent microseconds — `isoformat()` omits them when they are
        // zero, and a fixed-precision `.000000` here is a real bug this catches
        // (it cost a settle once already, on the intent side).
        Some("2026-09-08T19:11:22+00:00")
    );
    // And it is what freshness is measured from: 31s later the Mac is gone.
    assert!(reported_state(&conn, at(29)).unwrap().is_some());
    assert!(reported_state(&conn, at(31)).unwrap().is_none());
}

#[test]
fn source_liveness_is_stored_as_python_would_dump_it() {
    // ⚠ Both tiers write this column during the cutover, and `serde_json`'s
    // defaults differ from `json.dumps` in the separators. Two spellings of one
    // value in one column is exactly what makes a later parity check report
    // drift that is not drift.
    //
    // The expected text is not this implementation's output: it is what
    // `capture_control.record_reported` wrote for these same arguments when run
    // under the repo's own Python on 2026-09-08. Note the key order — the Mac's,
    // not serde's alphabetical one, which is why `preserve_order` is on.
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
    // A Mac too old to send the field must report no liveness, not break the
    // exchange — and `{}` is what the Python writes for it.
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
    // The Mac reads `pausedUntil`; a snake_case field or an omitted null would
    // read as "no pause" on a mirror that cannot tell the difference.
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

// --- the route is MOUNTED, not merely written --------------------------------

/// A router with the meaning plane's schema in place, and the sync gate either
/// configured or not.
async fn serve(token: Option<&str>) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    recalld::store::open(&root).expect("ingest db");
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
        // The sync plane is exempt from the SSO gate, so this is deliberately
        // left absent: the test would otherwise not be showing that.
        webauth: None,
        sync_token: token.map(ToOwned::to_owned),
        // No upstream, so an unmounted route is an honest 404 rather than a
        // proxy error — which is what lets the "not mounted" case be asserted.
        upstream: None,
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
    let url = format!("http://{addr}/sync/capture");
    let token = token.map(ToOwned::to_owned);
    tokio::task::spawn_blocking(move || {
        let mut req = ureq::post(&url);
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.send_json(body) {
            Ok(res) => (res.status(), res.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, res)) => (code, res.into_string().unwrap_or_default()),
            Err(err) => panic!("transport: {err}"),
        }
    })
    .await
    .expect("request")
}

/// ⚠ Written because of a real incident: on 2026-09-07 `/api/correct` was
/// written, tested, its Python deleted, and mounted NOWHERE — both suites green,
/// the endpoint served by nobody. Passing unit tests say nothing about reach.
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
    // The point of the exchange: the Mac's word must reach the state the
    // household's own status page serves, or a pause nobody can confirm is all
    // the UI ever shows.
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
    // ⚠ Not just "it 401s": the write must not have happened. A gate that
    // rejects the response after doing the work is not a gate.
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
    // ⚠ The inversion this repo makes deliberately for household data: an
    // unconfigured credential does NOT mean "run open" here. With no upstream
    // configured that shows up as a 404 — and in the pod, as the request going
    // to Python instead, which is what makes the cutover a one-line env change.
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
    // The hang is only for an UNCHANGED intent. A Mac that asks with a stale
    // `knownIntent` must be told immediately, not held for the cap — that delay
    // is the difference between a pause reaching the mic in ~RTT and in 25s.
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
    // ⚠ The property the whole hang exists for, and the one a unit test cannot
    // show: a pause pressed on the fleet's UI must reach the one-way peer
    // WITHOUT the peer having to poll again. Isis cannot dial the Mac, so if
    // this returns stale, a pause waits out the mirror's whole interval.
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
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "woke after {:?}, which is not ~one slice",
        started.elapsed()
    );
}
