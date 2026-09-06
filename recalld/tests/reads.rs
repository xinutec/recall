//! Stage F1's read routes, against a database built to hold the cases that
//! actually bite, plus the mounting rules.
//!
//! These were checked once against the live Python on a snapshot of the real
//! archive (10 cases, byte identical) and that harness was then retired, because
//! the product is being rebuilt rather than transported and a byte-parity gate
//! would fail on the first deliberate improvement — `clamp` below is already one.
//! What remains here are the invariants that must hold everywhere, including on
//! a fresh clone.

use recalld::reads;
use rusqlite::Connection;

/// The subset of `recall.sqlite` these routes read. Copied from
/// `recall.store_schema`, not imported, for the same reason `audiod::store`
/// copies its SQL: the Python owns the schema, and a test that re-derived it
/// would be testing its own copy rather than the shape on disk.
fn schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY, source_id TEXT NOT NULL, path TEXT NOT NULL,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL,
            sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL);
         CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            language TEXT, asr_confidence REAL, asr_model TEXT, loudness REAL,
            speaker_label TEXT, speaker_id INTEGER, speaker_guess TEXT,
            speaker_score REAL, speaker_cluster TEXT, superseded_by INTEGER,
            provenance TEXT, hidden_reason TEXT, word_timings TEXT);
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');",
    )
    .expect("schema");
}

#[allow(clippy::too_many_arguments)]
fn turn(conn: &Connection, id: i64, start: &str, text: &str, extra: &[(&str, &str)]) {
    conn.execute(
        "INSERT INTO transcript_segments (id, start_utc, end_utc, text) VALUES (?1, ?2, ?2, ?3)",
        (id, start, text),
    )
    .expect("turn");
    for (col, val) in extra {
        conn.execute(
            &format!("UPDATE transcript_segments SET {col} = ?1 WHERE id = ?2"),
            (*val, id),
        )
        .expect("extra");
    }
    conn.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (id, text),
    )
    .expect("fts");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("db");
    schema(&conn);
    conn
}

#[test]
fn a_superseded_or_hidden_turn_is_never_shown() {
    // The single most important property of the read plane: supersession and
    // soft-hiding are how this system corrects itself WITHOUT deleting, so a
    // reader that ignored them would resurrect every wrong transcript ever
    // written and every swept hallucination.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "current", &[]);
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "old",
        &[("superseded_by", "1")],
    );
    turn(
        &conn,
        3,
        "2026-09-01T10:00:02+00:00",
        "junk",
        &[("hidden_reason", "hallucination")],
    );

    let page = reads::timeline(&conn, 50, None).expect("timeline");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].text, "current");

    let hits = reads::search(&conn, "old OR junk OR current", 50).expect("search");
    assert_eq!(hits.items.len(), 1, "search must apply the same projection");
    assert_eq!(hits.items[0].text, "current");
}

#[test]
fn a_human_label_wins_and_drops_the_score_a_guess_keeps_it() {
    // The UI renders "Alice 31%" for a guess and a bare name for a confirmation.
    // Collapsing the two would either hide useful weak guesses or present a
    // machine guess as though a person had confirmed it.
    let conn = db();
    turn(
        &conn,
        1,
        "2026-09-01T10:00:00+00:00",
        "confirmed",
        &[("speaker_label", "Alex"), ("speaker_score", "0.9")],
    );
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "guessed",
        &[("speaker_guess", "Sam"), ("speaker_score", "0.31")],
    );

    let page = reads::timeline(&conn, 50, None).expect("timeline");
    let confirmed = &page.items[0];
    let guessed = &page.items[1];

    assert_eq!(confirmed.speaker.as_deref(), Some("Alex"));
    assert!(confirmed.speaker_confirmed);
    assert_eq!(
        confirmed.speaker_confidence, None,
        "a confirmed speaker carries no score — there is nothing to be unsure about"
    );

    assert_eq!(guessed.speaker.as_deref(), Some("Sam"));
    assert!(!guessed.speaker_confirmed);
    assert_eq!(guessed.speaker_confidence, Some(0.31));
}

#[test]
fn the_tier_badge_reports_how_much_processing_a_turn_has_had() {
    let conn = db();
    turn(
        &conn,
        1,
        "2026-09-01T10:00:00+00:00",
        "a",
        &[("asr_model", "human")],
    );
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "b",
        &[("asr_model", "live")],
    );
    turn(
        &conn,
        3,
        "2026-09-01T10:00:02+00:00",
        "c",
        &[("provenance", "diarized (mlx-whisper)")],
    );
    turn(
        &conn,
        4,
        "2026-09-01T10:00:03+00:00",
        "d",
        &[("asr_model", "mlx-whisper")],
    );

    let tiers: Vec<&str> = reads::timeline(&conn, 50, None)
        .expect("timeline")
        .items
        .iter()
        .map(|i| i.tier)
        .collect();
    assert_eq!(tiers, ["corrected", "live", "diarized", "transcribed"]);
}

#[test]
fn a_page_boundary_never_splits_turns_that_share_an_instant() {
    // Co-located microphones record the SAME speech, so several turns genuinely
    // carry one start time. A page that cut such a group in half would make the
    // next strict-`<` page skip the remainder — audio silently missing from the
    // timeline, which is the failure this whole system exists to avoid.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "older", &[]);
    for id in 2..=4 {
        turn(&conn, id, "2026-09-01T10:00:05+00:00", "tied", &[]);
    }

    // limit 2 lands the boundary inside the three-way tie.
    let page = reads::timeline(&conn, 2, None).expect("timeline");
    let tied = page.items.iter().filter(|i| i.text == "tied").count();
    assert_eq!(tied, 3, "the tie group must not be split across pages");
    assert!(page.has_more, "an extended page still has more behind it");

    // Paging on from the tie's instant reaches the older turn exactly once.
    let next = reads::timeline(&conn, 2, Some("2026-09-01T10:00:05+00:00")).expect("next");
    assert_eq!(next.items.len(), 1);
    assert_eq!(next.items[0].text, "older");
}

#[test]
fn a_page_reads_oldest_first_though_the_query_is_newest_first() {
    let conn = db();
    for (id, minute) in [(1, "00"), (2, "01"), (3, "02")] {
        turn(
            &conn,
            id,
            &format!("2026-09-01T10:{minute}:00+00:00"),
            "t",
            &[],
        );
    }
    let page = reads::timeline(&conn, 50, None).expect("timeline");
    let ids: Vec<i64> = page.items.iter().map(|i| i.id).collect();
    assert_eq!(ids, [1, 2, 3], "the page reads top-to-bottom in time order");
    assert!(!page.has_more);
}

#[test]
fn a_turn_with_no_audio_segment_still_appears() {
    // Corrections can exist with no audio row. An INNER join would drop exactly
    // the turns a person took the trouble to fix.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "corrected", &[]);
    let page = reads::timeline(&conn, 50, None).expect("timeline");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source, None);
}

#[test]
fn an_empty_page_is_not_treated_as_a_full_one() {
    // Guards the limit-0 edge: Python skips its tie pass on an empty page, and a
    // bare `len == limit` here would run one with no boundary.
    let conn = db();
    let page = reads::timeline(&conn, 0, None).expect("timeline");
    assert!(page.items.is_empty());
}

// --- mounting: the browsing plane exists only behind the gate ------------------

use axum::body::Body;
use axum::http::Request;
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::webauth::{self, COOKIE_NAME, GateState};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-secret-not-a-real-one";
const NOW: i64 = 1_788_000_000;

fn gate_state() -> GateState {
    GateState {
        cfg: Arc::new(webauth::Config {
            session_secret: SECRET.into(),
            client_id: "cid".into(),
            client_secret: "csec".into(),
            nc_base_url: "https://dash.example.org".into(),
            nc_internal_url: "https://dash.example.org".into(),
            redirect_uri: "http://10.100.0.2:8000/auth/callback".into(),
            allowed_users: std::collections::HashSet::new(),
            device_token: None,
        }),
        now: Arc::new(|| NOW),
    }
}

fn app(root: &std::path::Path, webauth: Option<GateState>) -> axum::Router {
    router(Arc::new(Config {
        root: root.to_path_buf(),
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        webauth,
    }))
}

#[tokio::test]
async fn an_unconfigured_recalld_does_not_serve_transcripts_at_all() {
    // ⚠ The one place this repo's inert-unless-configured rule is INVERTED, and
    // the inversion is the point. Everywhere else an absent credential means "run
    // open", which is right for a LAN-only dev box. These routes serve household
    // transcripts, so absent must mean the route does not exist — 404, not 200.
    let dir = tempfile::tempdir().expect("tempdir");
    let a = app(dir.path(), None);
    for path in ["/api/timeline", "/api/search?q=x"] {
        let code = a
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .expect("call")
            .status();
        assert_eq!(code, 404, "{path} must not exist without the gate");
    }

    // The ingest plane is unaffected — it has its own credential and its own rules.
    let code = a
        .oneshot(
            Request::get("/ingest/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("call")
        .status();
    assert_eq!(code, 200);
}

#[tokio::test]
async fn mounted_transcripts_are_refused_without_a_session_and_served_with_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    schema(&conn);
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "hello", &[]);
    drop(conn);

    let a = app(dir.path(), Some(gate_state()));

    // No cookie: the gate refuses before a single row is read.
    let code = a
        .clone()
        .oneshot(Request::get("/api/timeline").body(Body::empty()).unwrap())
        .await
        .expect("call")
        .status();
    assert_eq!(code, 401);

    // With a valid session the real query runs against the real database.
    let token = webauth::make_session_cookie(
        SECRET,
        &webauth::Session {
            user_id: "pippijn".into(),
            display_name: "Pippijn".into(),
        },
        NOW,
    )
    .expect("sign");
    let resp = a
        .oneshot(
            Request::get("/api/timeline")
                .header("cookie", format!("{COOKIE_NAME}={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("call");
    assert_eq!(resp.status(), 200);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    let page: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(page["items"][0]["text"], "hello");
}

#[tokio::test]
async fn a_limit_is_clamped_rather_than_trusted() {
    // The Python takes it straight from the query string, so ?limit=10000000
    // asks SQLite for the whole archive in one page. A browsing route a signed-in
    // person can accidentally turn into an archive dump will eventually be turned
    // into one, so this port clamps.
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    schema(&conn);
    for id in 1..=5 {
        turn(
            &conn,
            id,
            &format!("2026-09-01T10:00:0{id}+00:00"),
            "t",
            &[],
        );
    }
    drop(conn);

    let token = webauth::make_session_cookie(
        SECRET,
        &webauth::Session {
            user_id: "p".into(),
            display_name: "P".into(),
        },
        NOW,
    )
    .expect("sign");
    let resp = app(dir.path(), Some(gate_state()))
        .oneshot(
            Request::get("/api/timeline?limit=99999999")
                .header("cookie", format!("{COOKIE_NAME}={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("call");
    assert_eq!(
        resp.status(),
        200,
        "a huge limit must still answer, clamped"
    );
}
