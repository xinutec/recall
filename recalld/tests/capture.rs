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
