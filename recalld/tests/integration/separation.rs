//! Two people, one archive: what one recorded, named or corrected never reaches
//! the other. The routes are read out of the router's source, so a route added
//! later without a case here fails the test rather than going unchecked.

use crate::fixture::Fleet;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::webauth::{self, COOKIE_NAME, GateState, Session};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "test-secret-not-a-real-one";
const NOW: i64 = 1_788_000_000;

/// What only A's recordings, names and devices contain.
const A_MARKS: &[&str] = &["marmalade", "Quentinella", "phone-a", "meeting-a1"];

/// Every `METHOD /path` the browsing plane serves, read from `app.rs`.
fn routes() -> BTreeSet<String> {
    let src = include_str!("../../src/app.rs");
    let from = src.find("fn labelling").expect("labelling");
    let to = src.find("pub fn prepare").expect("prepare");
    let mut out = BTreeSet::new();
    let mut rest = &src[from..to];
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        let open = rest.find('"').expect("a path");
        let close = open + 1 + rest[open + 1..].find('"').expect("its end");
        let path = &rest[open + 1..close];
        let end = [".route(", ".with_state", ".merge(", ".layer("]
            .iter()
            .filter_map(|m| rest[close..].find(m))
            .min()
            .map_or(rest.len(), |n| close + n);
        let handlers = &rest[close..end];
        for method in ["get", "post", "put", "patch", "delete"] {
            let called = handlers.match_indices(&format!("{method}(")).any(|(i, _)| {
                !handlers[..i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            });
            if called {
                out.insert(format!("{} {path}", method.to_uppercase()));
            }
        }
    }
    out
}

/// A's ids, for the requests aimed at A's things.
#[derive(Clone, Copy)]
struct A {
    line: i64,
    line2: i64,
    line3: i64,
    line4: i64,
    /// The meeting's one line.
    meeting: i64,
    /// A's naming of a voice.
    edit: i64,
    /// A's glossary term.
    term: i64,
}

/// A's clip whose transcript loops over speech: a re-transcription candidate.
const LOOPED: &str = "phone-a-20260101T081000.flac";
/// A's first clip.
const A_CLIP: &str = "phone-a-20260101T080000.flac";
/// A clip that does not exist.
const NOWHERE: &str = "phone-z-20260101T080000.flac";

/// A's phone with four lines and a meeting, B's phone with one line. A named a
/// voice while correcting a line, marked one as no speech and excluded another,
/// through the routes; A's phone reported its status.
async fn world() -> (Fleet, A) {
    let f = Fleet::new();
    let speech = [
        (1.0, 5.0, "marmalade on the sill"),
        (10.0, 14.0, "marmalade again please"),
        (20.0, 24.0, "marmalade a third time"),
        (30.0, 34.0, "marmalade a fourth time"),
    ];
    let one_voice: &[(&str, f64, f64)] = &[("SPEAKER_00", 0.0, 60.0)];
    f.clip(
        "phone-a",
        "tcp_pcm",
        "20260101T080000",
        &speech,
        Some(one_voice),
    );
    let meeting = [(1.0, 5.0, "marmalade in the meeting")];
    f.clip(
        "meeting-a1",
        "upload",
        "20260101T090000",
        &meeting,
        Some(one_voice),
    );
    let looped = ["marmalade"; 30].join(" ");
    let looped = [(0.0, 20.0, looped.as_str())];
    f.clip("phone-a", "tcp_pcm", "20260101T081000", &looped, None);
    f.ingest
        .execute(
            "INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc, regions)
             VALUES (?1, 'phone-a', 20.0, '2026-01-01T08:12:00Z', '[[0.0,20.0]]')",
            [LOOPED],
        )
        .expect("speech under the loop");
    let b_speech = [(1.0, 5.0, "biscuit for b")];
    f.clip("phone-b", "tcp_pcm", "20260101T100000", &b_speech, None);
    f.meaning
        .execute_batch(
            "UPDATE sources SET owner = 'a' WHERE id IN ('phone-a', 'meeting-a1');
             UPDATE sources SET owner = 'b' WHERE id = 'phone-b';",
        )
        .expect("owners");
    f.refresh();
    // Unsure enough to be offered for review.
    f.meaning
        .execute(
            "UPDATE rendered_lines SET asr_confidence = 0.1 WHERE text LIKE '%in the meeting%'",
            [],
        )
        .expect("unsure");
    let line = |text: &str| -> i64 {
        f.meaning
            .query_row(
                "SELECT id FROM rendered_lines WHERE text LIKE ?1 AND hidden_reason IS NULL",
                [format!("%{text}%")],
                |r| r.get(0),
            )
            .expect(text)
    };
    let a = A {
        line: line("on the sill"),
        line2: line("again please"),
        line3: line("a third time"),
        line4: line("a fourth time"),
        meeting: line("in the meeting"),
        edit: 0,
        term: 0,
    };
    act_as_a(&f, &a).await;
    report_from_a(&f);
    let a = after_acting(&f, a);
    (f, a)
}

/// A's ids once A has acted: the naming, the term, and the corrected line,
/// which is a new line.
fn after_acting(f: &Fleet, a: A) -> A {
    let edit: i64 = f
        .meaning
        .query_row(
            "SELECT max(id) FROM edits WHERE json_extract(act, '$.Speaker') IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("A's naming");
    // The correction is a new line: undoing it means its id.
    let corrected: i64 = f
        .meaning
        .query_row(
            "SELECT id FROM rendered_lines WHERE tier = 'corrected' AND text LIKE '%windowsill%'",
            [],
            |r| r.get(0),
        )
        .expect("A's corrected line");
    let term: i64 = f
        .meaning
        .query_row(
            "SELECT id FROM vocabulary WHERE term = 'marmalade jam'",
            [],
            |r| r.get(0),
        )
        .expect("A's term");
    A {
        edit,
        term,
        line: corrected,
        ..a
    }
}

/// A names a voice while correcting a line, marks one as no speech, excludes
/// another and asks for a re-transcription, through the routes.
async fn act_as_a(f: &Fleet, a: &A) {
    let acts = [
        (
            "/api/correct",
            json!({"id": a.line, "text": "marmalade on the windowsill", "speaker": "Quentinella"}),
        ),
        ("/api/no-speech", json!({"id": a.line2})),
        ("/api/exclude", json!({"from": a.line4, "to": a.line4})),
        // Waiting, so A has a re-transcription to take back.
        ("/api/retranscribe", json!({"filenames": [A_CLIP]})),
        ("/api/vocabulary", json!({"term": "marmalade jam"})),
    ];
    for (uri, body) in acts {
        let (status, text) = call(f, "a", "POST", uri, Some(body)).await;
        assert!(status.is_success(), "A's own {uri}: {status} {text}");
    }
}

/// A's phone's heartbeat and upload outbox.
fn report_from_a(f: &Fleet) {
    let now = chrono::Utc::now();
    let beat = recalld::devices::Beat {
        device: "phone-a".into(),
        app: "recall-mic".into(),
        version: "1".into(),
        started_at: None,
        streaming: true,
        charging: None,
        mic_ok: None,
        via_lan: None,
        dropped_bytes: None,
        at: now.to_rfc3339(),
    };
    recalld::devices::record_beat(&f.meaning, &beat, now).expect("beat");
    let report = recalld::devices::Report {
        device: "phone-a".into(),
        queued: 1,
        oldest_queued_at: None,
        failing: 0,
        reason: None,
        at: now.to_rfc3339(),
    };
    recalld::devices::record_report(&f.meaning, &report, now).expect("report");
}

fn app(f: &Fleet) -> axum::Router {
    recalld::app::prepare(f.root()).expect("planes");
    let tokens = Arc::new(
        recalld::tokens::Tokens::parse("phone-a token-a\nphone-b token-b\n").expect("tokens"),
    );
    router(Arc::new(Config {
        root: f.root().to_path_buf(),
        tokens: Some(tokens.clone()),
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        trusted_proxies: Vec::new(),
        webauth: Some(GateState {
            cfg: Arc::new(webauth::Config {
                session_secret: SECRET.into(),
                client_id: "cid".into(),
                client_secret: "csec".into(),
                nc_base_url: "https://dash.example.org".into(),
                nc_internal_url: "https://dash.example.org".into(),
                redirect_uri: "http://localhost/auth/callback".into(),
                allowed_users: ["a", "b"].map(str::to_owned).into(),
                device_token: None,
            }),
            now: Arc::new(|| NOW),
            recorders: Some(Arc::new(webauth::Recorders {
                tokens,
                root: f.root().to_path_buf(),
            })),
        }),
        sync_token: None,
        frontend: None,
    }))
}

fn cookie(user: &str) -> String {
    let session = Session {
        user_id: user.into(),
        display_name: user.into(),
    };
    let token = webauth::make_session_cookie(SECRET, &session, NOW).expect("sign");
    format!("{COOKIE_NAME}={token}")
}

async fn call(
    f: &Fleet,
    user: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", cookie(user));
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    let request = request.body(body).expect("request");
    let response = app(f).oneshot(request).await.expect("call");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// What a route is, for this test.
enum Case {
    /// A sees their own data here; B must not.
    Read(String),
    /// One of A's things by id: B is told it does not exist, A is not (the
    /// fixture has no audio files, so A's answer need not be a success).
    Owned(String),
    /// Aimed at A's thing: B is refused as if it did not exist, A succeeds.
    Write(String, Option<serde_json::Value>),
    /// A batch of names: B's answer about A's clip is the answer about a clip
    /// that does not exist, word for word, and A's jobs stay as they were;
    /// A succeeds. The second body names nothing that exists.
    Absent(String, serde_json::Value, serde_json::Value),
    /// Not one person's data, and why.
    Global(&'static str),
}

/// The request each route gets. Every route in `app.rs` must have one.
fn case(route: &str, a: &A) -> Option<Case> {
    use Case::{Absent, Global, Owned, Read, Write};
    let session = |tail: &str| format!("/api/sessions/meeting-a1{tail}");
    Some(match route {
        "GET /api/timeline" => Read("/api/timeline".into()),
        "GET /api/conversations" => Read("/api/conversations".into()),
        "GET /api/search" => Read("/api/search?q=marmalade".into()),
        "GET /api/transcripts" => Read(format!("/api/transcripts?ids={}", a.line)),
        "GET /api/review" => Read("/api/review".into()),
        "GET /api/sources" => Read("/api/sources".into()),
        "GET /api/sessions" => Read("/api/sessions".into()),
        "GET /api/sessions/{source}/transcript" => Read(session("/transcript")),
        "GET /api/speakers" => Read("/api/speakers".into()),
        "GET /api/corrections" => Read("/api/corrections".into()),
        "GET /api/devices/heartbeat" => Read("/api/devices/heartbeat".into()),
        "GET /api/devices/outbox" => Read("/api/devices/outbox".into()),
        "GET /api/retranscribe" => Read("/api/retranscribe".into()),
        "GET /api/audio/{id}" => Owned(format!("/api/audio/{}", a.line)),
        "GET /api/audio-span" => Owned(format!(
            "/api/audio-span?from_id={}&to_id={}",
            a.line, a.line3
        )),
        "GET /api/correction/{id}/audio" => Owned(format!("/api/correction/{}/audio", a.edit)),
        "POST /api/correct" => Write(
            "/api/correct".into(),
            Some(json!({"id": a.line3, "text": "changed"})),
        ),
        "POST /api/correct/undo" => Write("/api/correct/undo".into(), Some(json!({"id": a.line}))),
        "POST /api/no-speech" => Write("/api/no-speech".into(), Some(json!({"id": a.line3}))),
        "POST /api/no-speech/undo" => {
            Write("/api/no-speech/undo".into(), Some(json!({"id": a.line2})))
        }
        "POST /api/exclude" => Write(
            "/api/exclude".into(),
            Some(json!({"from": a.line3, "to": a.line3})),
        ),
        "POST /api/exclude/undo" => Write("/api/exclude/undo".into(), Some(json!({"id": a.line4}))),
        "POST /api/correction/{id}/speaker" => Write(
            format!("/api/correction/{}/speaker", a.edit),
            Some(json!({"speaker": "Someone"})),
        ),
        "POST /api/correction/{id}/hide" => Write(format!("/api/correction/{}/hide", a.edit), None),
        "POST /api/retranscribe" => Absent(
            "/api/retranscribe".into(),
            json!({"filenames": [LOOPED]}),
            json!({"filenames": [NOWHERE]}),
        ),
        "POST /api/retranscribe/undo" => Absent(
            "/api/retranscribe/undo".into(),
            json!({"filename": A_CLIP}),
            json!({"filename": NOWHERE}),
        ),
        "PATCH /api/sessions/{source}" => Write(session(""), Some(json!({"title": "taken"}))),
        "DELETE /api/sessions/{source}" => Write(session(""), None),
        "POST /api/sessions/{source}/rediarize" => Write(session("/rediarize"), None),
        "POST /api/sessions/{source}/language" => {
            Write(session("/language"), Some(json!({"language": "en"})))
        }
        "POST /api/sessions/{source}/voice" => Write(
            session("/voice"),
            Some(json!({"cluster": "SPEAKER_00", "name": "Someone"})),
        ),
        "POST /api/sessions/{source}/assign" => Write(
            session("/assign"),
            Some(
                json!({"startTurn": a.meeting, "startChar": 0, "endTurn": a.meeting, "endChar": 3, "name": "Someone"}),
            ),
        ),
        "DELETE /api/devices/heartbeat/{device}" => {
            Write("/api/devices/heartbeat/phone-a".into(), None)
        }
        "DELETE /api/devices/outbox/{device}" => Write("/api/devices/outbox/phone-a".into(), None),
        "POST /api/sessions" => Global("an upload makes the uploader's own session"),
        "GET /api/vocabulary" => Read("/api/vocabulary".into()),
        "DELETE /api/vocabulary/{id}" => Write(format!("/api/vocabulary/{}", a.term), None),
        "POST /api/vocabulary" => {
            Global("adds to the asker's own glossary; GET shows only one's own")
        }
        "POST /api/devices/heartbeat" | "POST /api/devices/outbox" => {
            Global("a recorder's own report, sent without a session")
        }
        "GET /api/capture" | "POST /api/capture/pause" | "POST /api/capture/resume" => {
            Global("each person's own pause: a_pause_is_the_pausers_own")
        }
        "POST /api/log" | "POST /api/telemetry" => Global("write-only logs"),
        _ => return None,
    })
}

fn leaks(text: &str) -> Vec<&'static str> {
    A_MARKS
        .iter()
        .copied()
        .filter(|m| text.contains(m))
        .collect()
}

/// Every stored line and how it stands, to see that nothing changed.
fn texts(f: &Fleet) -> Vec<(String, Option<String>, Option<String>)> {
    f.meaning
        .prepare("SELECT text, hidden_reason, speaker_label FROM rendered_lines ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect")
}

#[tokio::test]
async fn nothing_of_one_persons_reaches_the_other() {
    let (probe, a) = world().await;
    let routes = routes();
    let unlisted: Vec<&String> = routes.iter().filter(|r| case(r, &a).is_none()).collect();
    assert!(
        unlisted.is_empty(),
        "routes with no case here: {unlisted:?}"
    );

    let mut failures = Vec::new();
    for route in &routes {
        let method = route.split(' ').next().expect("method");
        match case(route, &a).expect("listed") {
            Case::Read(uri) => {
                let (mine, as_a) = call(&probe, "a", method, &uri, None).await;
                if !mine.is_success() || leaks(&as_a).is_empty() {
                    failures.push(format!("{route}: A does not see their own ({mine})"));
                }
                let (_, as_b) = call(&probe, "b", method, &uri, None).await;
                if !leaks(&as_b).is_empty() {
                    failures.push(format!("{route}: B sees {:?}", leaks(&as_b)));
                }
            }
            Case::Owned(uri) => {
                let (b, _) = call(&probe, "b", method, &uri, None).await;
                let (mine, _) = call(&probe, "a", method, &uri, None).await;
                if b != StatusCode::NOT_FOUND {
                    failures.push(format!("{route}: B gets {b}, not 404"));
                }
                if mine == StatusCode::NOT_FOUND {
                    failures.push(format!("{route}: A is told it does not exist"));
                }
            }
            Case::Write(uri, body) => {
                let (theirs, _) = world().await;
                let before = texts(&theirs);
                let (b, _) = call(&theirs, "b", method, &uri, body.clone()).await;
                if b != StatusCode::NOT_FOUND {
                    failures.push(format!("{route}: B gets {b}, not 404"));
                }
                if texts(&theirs) != before {
                    failures.push(format!("{route}: B's request changed A's lines"));
                }
                let (own, _) = world().await;
                let (mine, text) = call(&own, "a", method, &uri, body).await;
                if !mine.is_success() {
                    failures.push(format!("{route}: A's own request fails ({mine} {text})"));
                }
            }
            Case::Absent(uri, theirs, nowhere) => {
                let (world_b, _) = world().await;
                let before = jobs(&world_b);
                let about_a = call(&world_b, "b", method, &uri, Some(theirs.clone())).await;
                let about_none = call(&world_b, "b", method, &uri, Some(nowhere)).await;
                // The answer echoes the name asked about; the rest must match.
                let unnamed = |(status, text): (StatusCode, String)| {
                    (
                        status,
                        text.replace(LOOPED, "NAME")
                            .replace(A_CLIP, "NAME")
                            .replace(NOWHERE, "NAME"),
                    )
                };
                let (about_a, about_none) = (unnamed(about_a), unnamed(about_none));
                if about_a != about_none {
                    failures.push(format!(
                        "{route}: B can tell A's clip from none: {about_a:?} vs {about_none:?}"
                    ));
                }
                if jobs(&world_b) != before {
                    failures.push(format!("{route}: B's request changed A's jobs"));
                }
                let (own, _) = world().await;
                let (mine, text) = call(&own, "a", method, &uri, Some(theirs)).await;
                if !mine.is_success() {
                    failures.push(format!("{route}: A's own request fails ({mine} {text})"));
                }
            }
            Case::Global(why) => assert!(!why.is_empty(), "{route}: a global route says why"),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every job and its state, to see that nothing was queued or cancelled.
fn jobs(f: &Fleet) -> Vec<(String, String)> {
    f.ingest
        .prepare("SELECT filename, state FROM jobs ORDER BY id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .collect::<Result<_, _>>()
        .expect("collect")
}

/// A request as a recorder sends it: its own token, no cookie.
async fn as_recorder(
    f: &Fleet,
    token: Option<&str>,
    request: axum::http::request::Builder,
    body: Body,
) -> (StatusCode, String) {
    let request = match token {
        Some(t) => request.header("authorization", format!("Bearer {t}")),
        None => request,
    };
    let response = app(f)
        .oneshot(request.body(body).expect("request"))
        .await
        .expect("call");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 24)
        .await
        .expect("body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A recorder speaks for whoever owns its source: its list of recorders is
/// that person's, a request with no credential sees none, and a meeting it
/// uploads is its owner's.
#[tokio::test]
async fn a_recorder_speaks_for_its_sources_owner() {
    let (f, _) = world().await;
    let list = |token| as_recorder(&f, token, Request::get("/api/sources"), Body::empty());
    let (status, mine) = list(Some("token-a")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        mine.contains("phone-a") && !mine.contains("phone-b"),
        "{mine}"
    );
    let (_, theirs) = list(Some("token-b")).await;
    assert!(
        theirs.contains("phone-b") && !theirs.contains("phone-a"),
        "{theirs}"
    );
    let (_, nobody) = list(None).await;
    assert!(!nobody.contains("phone-"), "{nobody}");
}

/// Each person's pause is their own: B pausing leaves A recording, and the
/// house, asked by no one in particular, keeps its own.
#[tokio::test]
async fn a_pause_is_the_pausers_own() {
    let (f, _) = world().await;
    let wants_running = |text: &str| {
        serde_json::from_str::<serde_json::Value>(text).expect("json")["desiredRunning"]
            .as_bool()
            .expect("desiredRunning")
    };
    let (status, text) = call(&f, "b", "POST", "/api/capture/pause", None).await;
    assert!(status.is_success(), "{status} {text}");

    let (_, b) = call(&f, "b", "GET", "/api/capture", None).await;
    assert!(!wants_running(&b), "B paused: {b}");
    let (_, a) = call(&f, "a", "GET", "/api/capture", None).await;
    assert!(wants_running(&a), "A untouched: {a}");
    let (_, house) = as_recorder(&f, None, Request::get("/api/capture"), Body::empty()).await;
    assert!(wants_running(&house), "the house untouched: {house}");
}
