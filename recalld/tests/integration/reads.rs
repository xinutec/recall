//! The read routes over the rendered view, against a database holding the hard
//! cases, and their mounting rules.

use recalld::line::Stage;
use recalld::reads;
use rusqlite::Connection;

/// The real `recall.sqlite` schema, built by the migration ladder.
fn schema(conn: &Connection) {
    recalld::meaning_schema::ensure(conn).expect("schema");
}

/// A rendered line; the search index follows by trigger.
fn turn(conn: &Connection, id: i64, start: &str, text: &str, extra: &[(&str, &str)]) {
    conn.execute(
        "INSERT INTO rendered_lines (id, start_utc, end_utc, text, asr_model, tier)
         VALUES (?1, ?2, ?2, ?3, 'whisper', 'transcribed')",
        (id, start, text),
    )
    .expect("turn");
    for (col, val) in extra {
        conn.execute(
            &format!("UPDATE rendered_lines SET {col} = ?1 WHERE id = ?2"),
            (*val, id),
        )
        .expect("extra");
    }
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("db");
    schema(&conn);
    conn
}

#[test]
fn a_hidden_line_is_never_shown() {
    // Model text render left out stays out of the read and the search.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "current", &[]);
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

    let hits = reads::search(&conn, "junk OR current", 50).expect("search");
    assert_eq!(hits.items.len(), 1, "search must apply the same projection");
    assert_eq!(hits.items[0].text, "current");
}

/// Punctuation in a vocabulary term is text, not query syntax: "Dr. Visser"
/// answered 500 because the full stop is an FTS5 syntax error.
#[test]
fn punctuation_in_a_search_is_text_and_the_operators_still_work() {
    let conn = db();
    turn(
        &conn,
        1,
        "2026-09-01T10:00:00+00:00",
        "dr. visser called",
        &[],
    );
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "jean-luc came by",
        &[],
    );
    turn(&conn, 3, "2026-09-01T10:00:02+00:00", "the plumber", &[]);

    let texts = |q: &str| -> Vec<String> {
        reads::search(&conn, q, 50)
            .unwrap_or_else(|e| panic!("{q:?}: {e}"))
            .items
            .into_iter()
            .map(|t| t.text)
            .collect()
    };
    assert_eq!(texts("Dr. Visser"), ["dr. visser called"]);
    assert_eq!(texts("Jean-Luc"), ["jean-luc came by"]);
    assert_eq!(texts("\"visser"), ["dr. visser called"]);
    assert_eq!(texts("plumb*"), ["the plumber"]);
    assert_eq!(texts("visser OR plumber").len(), 2);
}

#[test]
fn asked_for_hidden_lines_a_page_shows_them_with_their_reason() {
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "current", &[]);
    turn(
        &conn,
        3,
        "2026-09-01T10:00:02+00:00",
        "thanks",
        &[("hidden_reason", "nobody spoke")],
    );
    let window = |hidden| reads::Window {
        hidden,
        ..reads::Window::default()
    };

    let all = reads::recent(&conn, 50, window(true)).expect("recent");
    let shown: Vec<(i64, Option<&str>)> = all
        .iter()
        .map(|s| (s.id, s.hidden_reason.as_deref()))
        .collect();
    assert_eq!(shown, [(3, Some("nobody spoke")), (1, None)]);

    let visible = reads::recent(&conn, 50, window(false)).expect("recent");
    assert_eq!(visible.iter().map(|s| s.id).collect::<Vec<_>>(), [1]);
}

#[test]
fn a_human_label_wins_and_drops_the_score_a_guess_keeps_it() {
    // A guess carries its score ("Alice 31%"); a person's name carries none.
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
fn the_tier_badge_is_the_stored_tier_and_an_unknown_one_fails_the_read() {
    let conn = db();
    for (id, tier) in [
        (1, "corrected"),
        (2, "live"),
        (3, "diarized"),
        (4, "transcribed"),
    ] {
        turn(
            &conn,
            id,
            &format!("2026-09-01T10:00:0{id}+00:00"),
            "t",
            &[("tier", tier)],
        );
    }
    let tiers: Vec<Stage> = reads::timeline(&conn, 50, None)
        .expect("timeline")
        .items
        .iter()
        .map(|i| i.tier)
        .collect();
    assert_eq!(
        tiers,
        [
            Stage::Corrected,
            Stage::Live,
            Stage::Diarized,
            Stage::Transcribed
        ]
    );
    // The app's spelling of each is the stored one.
    for tier in tiers {
        assert_eq!(
            serde_json::to_value(tier).unwrap(),
            serde_json::Value::from(tier.as_str())
        );
    }

    turn(&conn, 5, "2026-09-01T10:00:05+00:00", "t", &[("tier", "?")]);
    assert!(reads::timeline(&conn, 50, None).is_err());
}

#[test]
fn a_page_boundary_never_splits_turns_that_share_an_instant() {
    // Several turns can share a start; a page cut inside the group would make
    // the next page skip the rest.
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
    // A live line has no audio row, and still shows.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "corrected", &[]);
    let page = reads::timeline(&conn, 50, None).expect("timeline");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source, None);
}

#[test]
fn an_empty_page_is_not_treated_as_a_full_one() {
    // At limit 0 the tie pass is skipped.
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
    recalld::app::prepare(root).expect("planes");
    router(Arc::new(Config {
        root: root.to_path_buf(),
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        trusted_proxies: Vec::new(),
        webauth,
        sync_token: None,
        frontend: None,
    }))
}

#[tokio::test]
async fn an_unconfigured_recalld_does_not_serve_transcripts_at_all() {
    // Unlike elsewhere, an absent credential here means no route (404), not an
    // open one: these serve household transcripts.
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

    // The ingest plane has its own credential.
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
            user_id: "user".into(),
            display_name: "Oskar".into(),
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
    // ?limit=10000000 is clamped.
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

#[test]
fn a_deep_link_reads_the_line_and_a_line_gone_since_answers_absent() {
    let conn = db();
    turn(&conn, 1, "2026-06-14T18:00:00+00:00", "heard a noise", &[]);

    let out = reads::current_version(&conn, 1)
        .expect("query")
        .expect("found");
    assert_eq!(out.text, "heard a noise");
    assert_eq!(reads::current_version(&conn, 2).expect("query"), None);
}

#[test]
fn ids_asked_for_twice_appear_once_in_the_order_asked() {
    let conn = db();
    turn(&conn, 1, "2026-06-14T18:00:00+00:00", "a", &[]);
    turn(&conn, 2, "2026-06-14T18:00:01+00:00", "b", &[]);

    let out = reads::transcripts(&conn, &[2, 1, 2, 9]).expect("query");

    let ids: Vec<i64> = out.items.iter().map(|i| i.id).collect();
    assert_eq!(ids, [2, 1]);
}

#[test]
fn the_review_queue_puts_unscored_turns_first() {
    // Unscored turns come first.
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    turn(
        &conn,
        1,
        "2026-06-14T18:00:00+00:00",
        "scored high",
        &[("asr_confidence", "0.8")],
    );
    turn(&conn, 2, "2026-06-14T18:00:01+00:00", "never scored", &[]);
    turn(
        &conn,
        3,
        "2026-06-14T18:00:02+00:00",
        "scored low",
        &[("asr_confidence", "0.2")],
    );

    let items = reads::review(&conn, 0.9, 50).expect("query").items;

    assert_eq!(
        items.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![2, 3, 1],
        "unscored, then least confident"
    );
}

#[test]
fn a_confident_turn_stays_out_of_the_review_queue() {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    turn(
        &conn,
        1,
        "2026-06-14T18:00:00+00:00",
        "sure",
        &[("asr_confidence", "0.95")],
    );

    assert!(
        reads::review(&conn, 0.9, 50)
            .expect("query")
            .items
            .is_empty()
    );
}

#[test]
fn a_line_says_whether_a_person_vouched_for_its_words() {
    // What lets Check skip a line wherever its words were checked, and only then.
    let conn = db();
    turn(
        &conn,
        1,
        "2026-09-01T10:00:00+00:00",
        "checked",
        &[("words_checked", "1")],
    );
    turn(&conn, 2, "2026-09-01T10:00:01+00:00", "not", &[]);
    let page = reads::timeline(&conn, 50, None).expect("timeline");
    let by_text: std::collections::BTreeMap<_, _> = page
        .items
        .iter()
        .map(|t| (t.text.as_str(), t.words_checked))
        .collect();
    assert_eq!(by_text.get("checked"), Some(&true));
    assert_eq!(by_text.get("not"), Some(&false));
}

#[test]
fn an_excluded_stretch_stays_on_the_page_so_it_can_be_taken_back_and_out_of_search() {
    // The app folds it into one row with an undo; other hidden lines stay
    // out unless asked for.
    let conn = db();
    turn(&conn, 1, "2026-09-01T10:00:00+00:00", "current", &[]);
    turn(
        &conn,
        2,
        "2026-09-01T10:00:01+00:00",
        "strangers",
        &[("hidden_reason", "excluded")],
    );
    turn(
        &conn,
        3,
        "2026-09-01T10:00:02+00:00",
        "thanks",
        &[("hidden_reason", "nobody spoke")],
    );

    let page = reads::timeline(&conn, 50, None).expect("timeline");
    let kinds: Vec<(String, Option<recalld::line::HiddenKind>)> = page
        .items
        .iter()
        .map(|t| (t.text.clone(), t.hidden_as))
        .collect();
    assert_eq!(
        kinds,
        [
            ("current".to_owned(), None),
            (
                "strangers".to_owned(),
                Some(recalld::line::HiddenKind::Excluded)
            ),
        ]
    );
    let hits = reads::search(&conn, "strangers OR current", 50).expect("search");
    assert_eq!(hits.items.len(), 1);
}
