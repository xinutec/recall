//! What the recorders say about themselves. Status, never control: a phone on an
//! older build must cost its own line and nothing else.

use recalld::devices::{Beat, Report, read_beats, read_reports, record_beat, record_report};
use rusqlite::Connection;

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
        .expect("schema");
    conn
}

fn beat(device: &str, at: &str) -> Beat {
    Beat {
        device: device.to_owned(),
        app: "recall-mic".into(),
        version: "1.2.3".into(),
        started_at: Some("2026-09-07T08:00:00+00:00".into()),
        streaming: true,
        charging: Some(true),
        mic_ok: Some(true),
        via_lan: Some(false),
        at: at.to_owned(),
    }
}

fn report(device: &str, at: &str) -> Report {
    Report {
        device: device.to_owned(),
        queued: 3,
        oldest_queued_at: Some("2026-09-07T07:00:00+00:00".into()),
        failing: 1,
        reason: Some("Not authorised — check the upload token".into()),
        at: at.to_owned(),
    }
}

fn stored(conn: &Connection, key: &str) -> String {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .expect("stored")
}

#[test]
fn a_beat_round_trips_and_replaces_the_previous_one() {
    let conn = db();

    record_beat(&conn, &beat("pixel9-aabbccdd", "2026-09-07T09:00:00+00:00")).expect("first");
    record_beat(&conn, &beat("pixel9-aabbccdd", "2026-09-07T10:00:00+00:00")).expect("second");

    let beats = read_beats(&conn).expect("read");
    assert_eq!(beats.len(), 1, "rewritten whole, not appended");
    assert_eq!(beats[0].at, "2026-09-07T10:00:00+00:00");
    assert_eq!(beats[0].device, "pixel9-aabbccdd");
    assert!(beats[0].streaming);
}

#[test]
fn beats_read_back_sorted_by_device_id() {
    let conn = db();
    for device in ["zz-phone", "aa-phone", "mm-phone"] {
        record_beat(&conn, &beat(device, "2026-09-07T09:00:00+00:00")).expect("beat");
    }

    let devices: Vec<String> = read_beats(&conn)
        .expect("read")
        .into_iter()
        .map(|b| b.device)
        .collect();

    assert_eq!(devices, ["aa-phone", "mm-phone", "zz-phone"]);
}

#[test]
fn a_malformed_entry_costs_that_device_its_line_and_no_more() {
    // ⚠ This is read on a health endpoint's request path. One phone on a broken
    // build must not blank the whole answer.
    let conn = db();
    record_beat(&conn, &beat("good-phone", "2026-09-07T09:00:00+00:00")).expect("beat");
    let mut map: serde_json::Value =
        serde_json::from_str(&stored(&conn, "device_mic_heartbeats")).expect("json");
    map["broken-phone"] = serde_json::json!({"app": "x"}); // no `at`
    map["not-even-a-map"] = serde_json::json!("nonsense");
    conn.execute(
        "UPDATE settings SET value = ?1 WHERE key = 'device_mic_heartbeats'",
        [map.to_string()],
    )
    .expect("write");

    let beats = read_beats(&conn).expect("read");

    assert_eq!(beats.len(), 1);
    assert_eq!(beats[0].device, "good-phone");
}

#[test]
fn an_unreadable_setting_reads_as_no_beats_rather_than_failing() {
    let conn = db();
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('device_mic_heartbeats', 'not json at all')",
        [],
    )
    .expect("write");

    assert!(read_beats(&conn).expect("read").is_empty());
}

#[test]
fn a_flood_of_devices_evicts_the_least_recently_heard() {
    // ⚠ The write endpoint is unauthenticated, so the device count is
    // client-controlled. Without the cap a stray test post needs sqlite3 surgery
    // in the pod to remove; with it, it ages out.
    let conn = db();
    for i in 0..20 {
        record_beat(
            &conn,
            &beat(
                &format!("phone-{i:02}"),
                &format!("2026-09-07T{i:02}:00:00+00:00"),
            ),
        )
        .expect("beat");
    }

    let beats = read_beats(&conn).expect("read");

    assert_eq!(beats.len(), 16, "capped");
    let kept: Vec<String> = beats.into_iter().map(|b| b.device).collect();
    assert!(
        !kept.contains(&"phone-00".to_owned()),
        "the oldest went first"
    );
    assert!(kept.contains(&"phone-19".to_owned()), "the newest stayed");
}

#[test]
fn an_entry_that_cannot_be_read_is_evicted_before_a_readable_one() {
    // It sorts oldest by having no `at` at all, which is what makes a malformed
    // row self-clearing rather than permanent.
    let conn = db();
    for i in 0..16 {
        record_beat(
            &conn,
            &beat(
                &format!("phone-{i:02}"),
                &format!("2026-09-07T{i:02}:00:00+00:00"),
            ),
        )
        .expect("beat");
    }
    let mut map: serde_json::Value =
        serde_json::from_str(&stored(&conn, "device_mic_heartbeats")).expect("json");
    map["junk-device"] = serde_json::json!({"app": "x"});
    conn.execute(
        "UPDATE settings SET value = ?1 WHERE key = 'device_mic_heartbeats'",
        [map.to_string()],
    )
    .expect("write");

    record_beat(&conn, &beat("phone-99", "2026-09-07T23:00:00+00:00")).expect("beat");

    let raw = stored(&conn, "device_mic_heartbeats");
    assert!(
        !raw.contains("junk-device"),
        "the unreadable entry went first"
    );
    assert!(raw.contains("phone-99"));
}

#[test]
fn the_outbox_is_not_capped_matching_the_python() {
    // ⚠ Deliberately mirrored rather than improved: the asymmetry with beats is a
    // known issue, and fixing it belongs with that issue rather than smuggled in.
    let conn = db();
    for i in 0..20 {
        record_report(
            &conn,
            &report(&format!("phone-{i:02}"), "2026-09-07T09:00:00+00:00"),
        )
        .expect("report");
    }

    assert_eq!(read_reports(&conn).expect("read").len(), 20);
}

#[test]
fn a_long_device_name_is_truncated_by_character_not_byte() {
    // A name of accented characters is 64 chars but more than 64 bytes; cutting
    // by byte would land mid-character.
    let conn = db();
    let long: String = "é".repeat(100);

    record_beat(&conn, &beat(&long, "2026-09-07T09:00:00+00:00")).expect("beat");

    let beats = read_beats(&conn).expect("read");
    assert_eq!(beats[0].device.chars().count(), 64);
}

#[test]
fn a_report_round_trips_with_its_reason_and_counts() {
    let conn = db();

    record_report(
        &conn,
        &report("pixel9-aabbccdd", "2026-09-07T09:00:00+00:00"),
    )
    .expect("report");

    let reports = read_reports(&conn).expect("read");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].queued, 3);
    assert_eq!(reports[0].failing, 1);
    assert_eq!(
        reports[0].reason.as_deref(),
        Some("Not authorised — check the upload token")
    );
}

#[test]
fn the_stored_json_is_spelled_the_way_python_writes_it() {
    // ⚠ Both implementations write this column while the port is in flight, and
    // the Python escapes non-ASCII and puts a space after `,` and `:`.
    let conn = db();
    let mut r = report("phone", "2026-09-07T09:00:00+00:00");
    r.reason = Some("kon niet uploaden — geërfde fout".into());

    record_report(&conn, &r).expect("report");

    let raw = stored(&conn, "device_outbox_reports");
    assert!(raw.contains("\", \""), "a space after the comma: {raw}");
    assert!(raw.contains("\": "), "a space after the colon");
    assert!(raw.contains("\\u00eb"), "non-ASCII escaped, got {raw}");
    assert!(!raw.contains('ë'), "and not written raw");
}

#[test]
fn a_beat_with_no_optional_flags_keeps_them_absent_rather_than_false() {
    // None means "an app too old to say", which is not the same as False.
    let conn = db();
    let mut b = beat("old-phone", "2026-09-07T09:00:00+00:00");
    b.charging = None;
    b.mic_ok = None;
    b.via_lan = None;
    b.started_at = None;

    record_beat(&conn, &b).expect("beat");

    let beats = read_beats(&conn).expect("read");
    assert_eq!(beats[0].charging, None);
    assert_eq!(beats[0].mic_ok, None);
    assert_eq!(beats[0].via_lan, None);
    assert_eq!(beats[0].started_at, None);
}

// --- through the router ------------------------------------------------------

use axum::body::Body;
use axum::http::Request;
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::webauth::{self, GateState};
use std::sync::Arc;
use tower::ServiceExt;

const NOW: i64 = 1_788_000_000;

fn gated(root: &std::path::Path) -> axum::Router {
    router(Arc::new(Config {
        root: root.to_path_buf(),
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        webauth: Some(GateState {
            cfg: Arc::new(webauth::Config {
                session_secret: "test-secret-not-a-real-one".into(),
                client_id: "cid".into(),
                client_secret: "csec".into(),
                nc_base_url: "https://dash.example.org".into(),
                nc_internal_url: "https://dash.example.org".into(),
                redirect_uri: "http://127.0.0.1/auth/callback".into(),
                allowed_users: std::collections::HashSet::new(),
                device_token: None,
            }),
            now: Arc::new(|| NOW),
        }),
        upstream: None,
        frontend: None,
    }))
}

fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    conn.execute_batch("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
        .expect("schema");
    recalld::store::open(dir.path()).expect("ingest db");
    dir
}

async fn post(app: &axum::Router, path: &str, body: serde_json::Value) -> axum::http::StatusCode {
    app.clone()
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .expect("call")
        .status()
}

#[tokio::test]
async fn a_beat_posts_without_a_session_because_a_phone_cannot_sign_in() {
    // ⚠ If this ever needs a cookie, every recorder goes silent and the fleet
    // reads a dead house.
    let dir = scratch();
    let app = gated(dir.path());

    let code = post(
        &app,
        "/api/devices/heartbeat",
        serde_json::json!({"device": "pixel9", "app": "recall-mic", "version": "1",
                           "streaming": true}),
    )
    .await;

    assert_eq!(code, 200);
}

#[tokio::test]
async fn the_beats_clock_is_the_servers_not_the_phones() {
    // ⚠ A phone with a wrong clock would otherwise report itself permanently
    // fresh, or permanently stale. The body carries no `at` at all, by design.
    let dir = scratch();
    let app = gated(dir.path());

    post(
        &app,
        "/api/devices/heartbeat",
        serde_json::json!({"device": "pixel9", "app": "recall-mic", "version": "1",
                           "streaming": true, "at": "1999-01-01T00:00:00+00:00"}),
    )
    .await;

    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let beats = read_beats(&conn).expect("read");
    assert_eq!(beats.len(), 1);
    assert!(
        !beats[0].at.starts_with("1999"),
        "the phone's clock must not set `at`, got {}",
        beats[0].at
    );
}

#[tokio::test]
async fn an_unparseable_time_from_a_phone_is_dropped_not_a_500() {
    // ⚠ Status, never control. An app on an older build costs its own DETAIL and
    // nothing else — least of all the beat, which is the part that matters.
    let dir = scratch();
    let app = gated(dir.path());

    let code = post(
        &app,
        "/api/devices/heartbeat",
        serde_json::json!({"device": "pixel9", "app": "recall-mic", "version": "1",
                           "streaming": true, "startedAt": "whenever"}),
    )
    .await;

    assert_eq!(code, 200, "the beat is still recorded");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let beats = read_beats(&conn).expect("read");
    assert_eq!(beats.len(), 1);
    assert_eq!(beats[0].started_at, None, "only the detail is lost");
}

#[tokio::test]
async fn an_unparseable_queue_time_costs_the_outbox_report_nothing_else() {
    let dir = scratch();
    let app = gated(dir.path());

    let code = post(
        &app,
        "/api/devices/outbox",
        serde_json::json!({"device": "pixel9", "queued": 4, "failing": 1,
                           "oldestQueuedAt": "not a time", "reason": "no token"}),
    )
    .await;

    assert_eq!(code, 200);
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let reports = read_reports(&conn).expect("read");
    assert_eq!(reports[0].queued, 4);
    assert_eq!(reports[0].oldest_queued_at, None);
}

#[tokio::test]
async fn negative_counts_are_clamped_rather_than_refused() {
    let dir = scratch();
    let app = gated(dir.path());

    post(
        &app,
        "/api/devices/outbox",
        serde_json::json!({"device": "pixel9", "queued": -5, "failing": -2}),
    )
    .await;

    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let reports = read_reports(&conn).expect("read");
    assert_eq!(reports[0].queued, 0);
    assert_eq!(reports[0].failing, 0);
}

#[tokio::test]
async fn reading_the_beats_requires_a_session_unlike_writing_them() {
    // The GET is fleetwatch's, which authenticates; only the phone's POST is
    // exempt. An open read would hand the device inventory to anyone on the VPN.
    let dir = scratch();
    let app = gated(dir.path());

    let code = app
        .oneshot(
            Request::get("/api/devices/heartbeat")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("call")
        .status();

    assert_eq!(code, 401);
}

#[tokio::test]
async fn a_beat_carrying_only_a_device_id_still_counts_as_alive() {
    // ⚠ Every field but `device` is optional in the Python, and the reason is the
    // point of the endpoint: an app on an older build must still register as
    // alive. Requiring app/version/streaming 422s exactly the phone this exists
    // to notice — which is what the Rust did until a Python test said otherwise.
    let dir = scratch();
    let app = gated(dir.path());

    let code = post(
        &app,
        "/api/devices/heartbeat",
        serde_json::json!({"device": "pixel5"}),
    )
    .await;

    assert_eq!(code, 200);
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let beats = read_beats(&conn).expect("read");
    assert_eq!(beats.len(), 1);
    assert_eq!(beats[0].app, "", "an absent app is empty, not a refusal");
    assert!(!beats[0].streaming, "and streaming defaults to false");
}

#[tokio::test]
async fn an_outbox_report_carrying_only_a_device_id_is_accepted() {
    let dir = scratch();
    let app = gated(dir.path());

    let code = post(
        &app,
        "/api/devices/outbox",
        serde_json::json!({"device": "pixel5"}),
    )
    .await;

    assert_eq!(code, 200);
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let reports = read_reports(&conn).expect("read");
    assert_eq!(reports[0].queued, 0);
    assert_eq!(reports[0].failing, 0);
}
