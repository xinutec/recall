//! The Mac→fleet sync plane: its gate, the capture handshake, and the routes
//! the Mac's agents read.

use recalld::capture::{intent_pause, record_reported, reported_state};
use recalld::sync::{IntentOut, bearer, check};
use rusqlite::Connection;

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
    // ⚠ The REAL ladder, not a hand-written subset. These routes are reached
    // over HTTP and answer 500 on any schema they did not expect, so a fixture
    // that approximates the schema tests the approximation — and a column this
    // file forgot would look exactly like an unmounted route.
    let conn = recalld::work::open_write(&root).expect("recall db");
    recalld::meaning_schema::ensure(&conn).expect("schema");
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
        let mut req = agent().post(&url);
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
    // An unconfigured credential does not mean "run open" here: the route is
    // not mounted, and a miss is a 404.
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
    // ⚠ **This bound is the measurement, not a formality** (#1490). Before the
    // notify this route slept a full 2 s slice, so a press landing 300 ms in was
    // not seen for ~1.7 s more; with the writer in this process it now wakes in
    // ~RTT. 500 ms fails on the slice and passes on the notify, which is exactly
    // the difference the fix exists to make.
    assert!(
        started.elapsed() < std::time::Duration::from_millis(500),
        "woke {:?} after the press — that is a slice, not a notify",
        started.elapsed()
    );
}

// --- lock contention ---------------------------------------------------------

/// ⚠ Written from a production failure. Within ten minutes of `/sync/capture`
/// cutting over on 2026-09-08, one mirror handshake in 116 answered 500 with
/// `database is locked` — where the Python it replaced had served 104,482 of
/// them without one. The cause was not the route: `work::open_write` gave up
/// after 5 s where the Python's `Store` waits 30, and the shorter side decides.
///
/// This asserts the WAIT, not the number, by holding the write lock for longer
/// than the old timeout and requiring the handshake to succeed anyway.
#[tokio::test]
async fn a_writer_holding_the_lock_delays_the_handshake_rather_than_failing_it() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let root = dir.path().to_path_buf();

    // A second writer takes the lock and keeps it past the old 5 s timeout.
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
    let url = format!("http://{addr}{path}");
    let token = token.map(ToOwned::to_owned);
    tokio::task::spawn_blocking(move || {
        let mut req = agent().get(&url);
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.call() {
            Ok(res) => (res.status(), res.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, res)) => (code, res.into_string().unwrap_or_default()),
            Err(err) => panic!("transport: {err}"),
        }
    })
    .await
    .expect("request")
}

/// ⚠ Every read route, checked for REACH and for its GATE in one pass — the
/// `/api/correct` incident was a handler that was written, tested, and mounted
/// nowhere, and an ungated one here would hand the household's names and its
/// glossary to anything that can reach the port.
#[tokio::test]
async fn every_sync_read_route_is_mounted_and_gated() {
    // ⚠ These routes answer 200 with an empty body when their tables are
    // missing, so a fixture that got the schema wrong would leave every
    // assertion below passing for the wrong reason — the exact shape this test
    // exists to catch. `serve` therefore runs the real migration ladder.
    let (_dir, addr) = serve(Some("sekrit")).await;

    for path in ["/sync/vocabulary/prompt"] {
        let (ok, body) = get(&addr, path, Some("sekrit")).await;
        assert_eq!(ok, 200, "{path} must be MOUNTED: {body}");

        for bad in [None, Some("wrong")] {
            let (refused, _) = get(&addr, path, bad).await;
            assert_eq!(refused, 401, "{path} must be GATED (token {bad:?})");
        }
    }
}

/// Without a token the whole plane is absent, not open — the same inversion the
/// capture handshake makes, and for the same reason: these carry the household's
/// names.
#[tokio::test]
async fn the_read_routes_are_absent_when_no_token_is_configured() {
    let (_dir, addr) = serve(None).await;

    for path in ["/sync/vocabulary/prompt", "/sync/live/health"] {
        let (status, _) = get(&addr, path, Some("sekrit")).await;
        assert_eq!(status, 404, "{path} must not answer at all");
    }
}

/// ⚠ **The third long-poll path, and the one neither language pinned.** The two
/// above cover "the intent already differs, return at once" and "hang, then wake
/// on a press". Nothing covered the wait simply ELAPSING with nothing to report,
/// which is what happens on most passes in a quiet house — the mirror polls,
/// nobody touches capture, and the route must come back with the unchanged
/// intent rather than hang to the cap or error.
///
/// Python bounds this with `_INTENT_WAIT_CAP_S`/`_INTENT_WAIT_SLICE_S` and the
/// Rust with `WAIT_CAP`/`WAIT_SLICE`; a divergence here would show up as the
/// Mac's capture mirror stalling for 25 s a pass instead of its own interval,
/// which reads as a slow network rather than as a bug (#1500).
#[tokio::test]
async fn a_wait_that_elapses_with_no_change_returns_the_intent_it_started_with() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    let intent = intent_pause(&conn, chrono::Utc::now(), Some(30)).unwrap();
    drop(conn);

    // `knownIntent` EQUALS what is stored, so there is nothing to report and the
    // route hangs until the wait runs out.
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

// --- the live tier's own numbers (#1671) -------------------------------------

/// `GET /sync/live/health` with the stamps spelled the way a caller sends them.
async fn live_health(addr: &str, token: Option<&str>, since: &str) -> (u16, String) {
    let url = format!("http://{addr}/sync/live/health");
    let (token, since) = (token.map(ToOwned::to_owned), since.to_owned());
    tokio::task::spawn_blocking(move || {
        let mut req = agent()
            .get(&url)
            .query("lag_since", &since)
            .query("window_since", &since)
            .query("window_until", "2026-09-21T12:00:00+00:00");
        if let Some(token) = token {
            req = req.set("Authorization", &format!("Bearer {token}"));
        }
        match req.call() {
            Ok(res) => (res.status(), res.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, res)) => (code, res.into_string().unwrap_or_default()),
            Err(err) => panic!("transport: {err}"),
        }
    })
    .await
    .expect("request")
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
    // ⚠⚠ Every stored timestamp is compared as TEXT, so `…Z` and `…+00:00` are
    // the same instant and two different VALUES — and `Z` (0x5A) sorts after
    // `+` (0x2B), so a caller that spelled its window the other way would
    // silently lose its own first row while still getting a well-formed 200.
    // The bound is therefore re-spelled server-side rather than trusted.
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

    // The turn ends exactly ON the boundary, which is where the two spellings
    // disagree; anywhere else the minutes differ first and the bug hides.
    for spelling in ["2026-09-21T11:40:00+00:00", "2026-09-21T11:40:00Z"] {
        let (status, body) = live_health(&addr, Some("sekrit"), spelling).await;
        assert_eq!(status, 200, "{spelling}: {body}");
        assert!(body.contains(r#""lagSamples":1"#), "{spelling}: {body}");
    }
}

#[tokio::test]
async fn a_window_bound_that_is_not_an_instant_is_refused() {
    // Not a 500, and not a silent comparison against a string that merely
    // looks like a timestamp.
    let (_dir, addr) = serve(Some("sekrit")).await;
    let (status, _) = live_health(&addr, Some("sekrit"), "yesterday").await;
    assert_eq!(status, 400);
}
