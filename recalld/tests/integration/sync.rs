//! The Mac→fleet sync plane: its gate, and the capture handshake's parity with
//! the Python it replaces.

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
    let (dir, addr) = serve(Some("sekrit")).await;
    let conn = recalld::work::open_write(dir.path()).expect("db");
    // ⚠ NOT `.ok()`. These routes answer 200 with an empty body when their tables
    // are missing, so a swallowed setup failure would leave every assertion below
    // passing for the wrong reason — the exact shape this test exists to catch.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS speakers (
             id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE);
         CREATE TABLE IF NOT EXISTS vocabulary (
             id INTEGER PRIMARY KEY, term TEXT NOT NULL UNIQUE, created_utc TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER, speaker_label TEXT,
             speaker_cluster TEXT, superseded_by INTEGER, hidden_reason TEXT);",
    )
    .expect("the meaning-plane schema these reads need");
    drop(conn);

    for path in [
        "/sync/labels",
        "/sync/vocabulary/prompt",
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

/// Without a token the whole plane is absent, not open — the same inversion the
/// capture handshake makes, and for the same reason: these carry the household's
/// names.
#[tokio::test]
async fn the_read_routes_are_absent_when_no_token_is_configured() {
    let (_dir, addr) = serve(None).await;

    for path in ["/sync/labels", "/sync/vocabulary/prompt"] {
        let (status, _) = get(&addr, path, Some("sekrit")).await;
        assert_eq!(status, 404, "{path} must not answer at all");
    }
}

// --- the audio blob plane, through the real router ----------------------------

/// Push a blob as multipart, the way the Mac's client does.
async fn push_blob(
    addr: &str,
    token: &str,
    source: &str,
    name: &str,
    bytes: Vec<u8>,
) -> (u16, String) {
    let url = format!("http://{addr}/sync/audio");
    let (token, source, name) = (token.to_owned(), source.to_owned(), name.to_owned());
    tokio::task::spawn_blocking(move || {
        let boundary = "----recalltestboundary";
        let mut body = Vec::new();
        for (field, value) in [("source", source.as_bytes()), ("name", name.as_bytes())] {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{field}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(&bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let res = agent()
            .post(&url)
            .set("Authorization", &format!("Bearer {token}"))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body);
        match res {
            Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
            Err(err) => panic!("transport: {err}"),
        }
    })
    .await
    .expect("request")
}

/// ⚠ **This is the test that catches a dead audio plane.** `app::router` layers
/// its body limit onto the ingest router only, and a `.layer` applies to routes
/// added BEFORE it — so the sync router, merged afterwards, silently inherits
/// axum's 2 MB default. Every real segment is single-digit MB and the largest
/// meeting in the archive is 62 MB, so without an explicit limit this plane
/// refuses everything the Mac sends and retries for ever.
///
/// 4 MB here: comfortably past the 2 MB default, small enough to stay a fast
/// test. It fails with 413 if the limit is ever lost.
#[tokio::test]
async fn a_push_larger_than_axums_default_body_limit_is_accepted() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let big = vec![7u8; 4 * 1024 * 1024];

    let (status, body) = push_blob(
        &addr,
        "sekrit",
        "usb",
        "usb-20260613T170653.opus",
        big.clone(),
    )
    .await;

    assert_eq!(status, 200, "a 4 MB push was refused: {body}");
    assert_eq!(body, r#"{"stored":true}"#);
    let landed = std::fs::read(dir.path().join("usb").join("usb-20260613T170653.opus")).unwrap();
    assert_eq!(landed.len(), big.len(), "the bytes did not all arrive");
}

/// ⚠ The archive is immutable: same path, same content. A re-push is the Mac
/// retrying after a timeout it cannot tell from a failure, and must never
/// overwrite — nor report a second store.
#[tokio::test]
async fn a_repushed_blob_is_not_stored_twice_and_is_never_overwritten() {
    let (dir, addr) = serve(Some("sekrit")).await;
    let name = "usb-20260613T170750.opus";

    assert_eq!(
        push_blob(&addr, "sekrit", "usb", name, b"first".to_vec())
            .await
            .1,
        r#"{"stored":true}"#
    );
    // Different bytes, same name — the archive must keep the first.
    let (status, body) = push_blob(&addr, "sekrit", "usb", name, b"second".to_vec()).await;

    assert_eq!(status, 200);
    assert_eq!(body, r#"{"stored":false}"#);
    assert_eq!(
        std::fs::read(dir.path().join("usb").join(name)).unwrap(),
        b"first",
        "an immutable blob was overwritten"
    );
}

/// The presence check is what lets the Mac skip re-sending bytes it already
/// delivered, so a wrong answer here costs the whole archive's bandwidth.
#[tokio::test]
async fn presence_and_fetch_round_trip_and_refuse_traversal() {
    let (_dir, addr) = serve(Some("sekrit")).await;
    let name = "usb-20260613T170850.opus";

    let absent = get(
        &addr,
        &format!("/sync/audio?source=usb&name={name}"),
        Some("sekrit"),
    )
    .await;
    assert_eq!(absent.1, r#"{"present":false}"#);

    push_blob(&addr, "sekrit", "usb", name, b"audio bytes".to_vec()).await;

    let present = get(
        &addr,
        &format!("/sync/audio?source=usb&name={name}"),
        Some("sekrit"),
    )
    .await;
    assert_eq!(present.1, r#"{"present":true}"#);

    let fetched = get(
        &addr,
        &format!("/sync/audio/file?source=usb&name={name}"),
        Some("sekrit"),
    )
    .await;
    assert_eq!(fetched.0, 200);
    assert_eq!(fetched.1, "audio bytes");

    // A missing one is a 404, not a 500 or an empty 200.
    let missing = get(
        &addr,
        "/sync/audio/file?source=usb&name=usb-20990101T000000.opus",
        Some("sekrit"),
    )
    .await;
    assert_eq!(missing.0, 404);

    // ⚠ And the guard holds over HTTP, not just in the unit test.
    let escape = get(
        &addr,
        "/sync/audio/file?source=..&name=recall.sqlite",
        Some("sekrit"),
    )
    .await;
    assert_eq!(escape.0, 400, "a traversal reached the filesystem");
}

/// Every audio route is gated — the blobs are the household's recordings.
#[tokio::test]
async fn the_audio_routes_are_gated() {
    let (_dir, addr) = serve(Some("sekrit")).await;

    assert_eq!(
        get(&addr, "/sync/audio?source=usb&name=x.opus", None)
            .await
            .0,
        401
    );
    assert_eq!(
        get(
            &addr,
            "/sync/audio/file?source=usb&name=x.opus",
            Some("wrong")
        )
        .await
        .0,
        401
    );
    assert_eq!(
        push_blob(&addr, "wrong", "usb", "x.opus", b"x".to_vec())
            .await
            .0,
        401
    );
}

// --- the segment push, through the real router --------------------------------

async fn post_json(
    addr: &str,
    path: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> (u16, String) {
    let url = format!("http://{addr}{path}");
    let token = token.map(ToOwned::to_owned);
    tokio::task::spawn_blocking(move || {
        let mut req = agent().post(&url);
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        match req.send_json(body) {
            Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
            Err(err) => panic!("transport: {err}"),
        }
    })
    .await
    .expect("request")
}

fn a_segment(text: &str) -> serde_json::Value {
    serde_json::json!({
        "source_id": "usb", "source_name": "USB mic", "kind": "coreaudio",
        "path": "/Volumes/Backup/recall/usb/usb-20260909T100000.opus",
        "start": "2026-09-09T10:00:00+00:00", "end": "2026-09-09T10:01:00+00:00",
        "sample_rate": 48000, "channels": 1,
        "turns": [{"start":"2026-09-09T10:00:00+00:00","end":"2026-09-09T10:00:10+00:00",
                   "text": text, "asr_model":"turbo","language":"en"}]
    })
}

/// The meaning-plane schema the segment push writes into.
fn seed_meaning_schema(root: &std::path::Path) {
    let conn = recalld::work::open_write(root).expect("db");
    conn.execute_batch(
        // ⚠ THE REAL SCHEMA. An invented `spec` column here — taken from the
        // Python DATACLASS, which has one, rather than the table, which does not
        // — is why every real push 500'd for fifteen minutes on 2026-09-09.
        "CREATE TABLE IF NOT EXISTS sources (
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
             port INTEGER, event_db REAL, noise_shape BLOB);
         CREATE TABLE IF NOT EXISTS audio_segments (
             id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
             start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, sample_rate INTEGER NOT NULL,
             channels INTEGER NOT NULL, transcribed_utc TEXT, UNIQUE (source_id, start_utc));
         CREATE TABLE IF NOT EXISTS transcript_segments (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER, start_utc TEXT NOT NULL,
             end_utc TEXT NOT NULL, text TEXT NOT NULL, language TEXT, asr_confidence REAL,
             asr_model TEXT NOT NULL, speaker_cluster TEXT, speaker_guess TEXT,
             speaker_score REAL, provenance TEXT, superseded_by INTEGER, hidden_reason TEXT);
         CREATE VIRTUAL TABLE IF NOT EXISTS transcript_fts USING fts5(text, content='');
         CREATE TABLE IF NOT EXISTS corrections (
             id INTEGER PRIMARY KEY, audio_segment_id INTEGER NOT NULL, start_utc TEXT NOT NULL,
             end_utc TEXT NOT NULL, corrected_text TEXT NOT NULL, language TEXT);
         CREATE TABLE IF NOT EXISTS deleted_segments (
             source_id TEXT NOT NULL, start_utc TEXT NOT NULL);",
    )
    .expect("schema");
}

#[tokio::test]
async fn the_segment_routes_are_mounted_and_gated() {
    let (dir, addr) = serve(Some("sekrit")).await;
    seed_meaning_schema(dir.path());

    let (ok, body) = post_json(&addr, "/sync/segments", Some("sekrit"), a_segment("hello")).await;
    assert_eq!(ok, 200, "the single push is not mounted: {body}");
    let out: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(out["turns_written"], 1);
    assert_eq!(out["tombstoned"], false);
    assert!(out["audio_segment_id"].as_i64().unwrap() > 0);

    for bad in [None, Some("wrong")] {
        assert_eq!(
            post_json(&addr, "/sync/segments", bad, a_segment("x"))
                .await
                .0,
            401
        );
        assert_eq!(
            post_json(
                &addr,
                "/sync/segments/batch",
                bad,
                serde_json::json!({"segments":[]})
            )
            .await
            .0,
            401
        );
    }
}

/// ⚠ Results are ALIGNED BY INDEX with the request — the Mac marks each id
/// pushed by position, so a reordered or short result list would advance the
/// watermark past segments that never landed.
#[tokio::test]
async fn the_batch_returns_one_result_per_segment_in_order() {
    let (dir, addr) = serve(Some("sekrit")).await;
    seed_meaning_schema(dir.path());

    let mut second = a_segment("second");
    second["start"] = serde_json::json!("2026-09-09T10:02:00+00:00");
    second["end"] = serde_json::json!("2026-09-09T10:03:00+00:00");
    second["path"] = serde_json::json!("/Volumes/Backup/recall/usb/usb-20260909T100200.opus");

    let (status, body) = post_json(
        &addr,
        "/sync/segments/batch",
        Some("sekrit"),
        serde_json::json!({"segments": [a_segment("first"), second]}),
    )
    .await;

    assert_eq!(status, 200, "{body}");
    let out: serde_json::Value = serde_json::from_str(&body).unwrap();
    let results = out["results"].as_array().expect("results");
    assert_eq!(results.len(), 2, "one result per segment, aligned by index");
    assert!(results.iter().all(|r| r["turns_written"] == 1));
    assert_ne!(
        results[0]["audio_segment_id"], results[1]["audio_segment_id"],
        "two distinct segments collapsed into one row"
    );
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
