//! The strangler fallback (stage F1).
//!
//! Driven against a REAL upstream server rather than a mocked client: the
//! likeliest error in a proxy is the request or response SHAPE — a header copied
//! that should not be, a status flattened, a body truncated — and a mock would
//! test my expectation of that shape instead of the shape itself. The same
//! reasoning as the OAuth exchange in `webauth`.

use axum::Router;
use axum::routing::{get, post};
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::proxy::{Upstream, forwarded_headers, target};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Turn a ureq result into a response, naming WHICH failure happened.
///
/// ⚠ **The instrument this test needed before any fix** (#1480). `proxy::forward`
/// answers 502 with three different bodies — `upstream unreachable` (ureq could
/// not talk to the stub), `proxy failed` (the `spawn_blocking` join died) and
/// `upstream body failed` (the relay could not read the body). They point at
/// three different causes, and a bare `.expect("call")` prints only the status
/// line, so an intermittent 502 in the gate could not say which it was. It is a
/// LOAD flake — the same derivation hash failed and then passed minutes later —
/// and knowing which of the three fires is what tells load from breakage.
fn answered(result: Result<ureq::Response, Box<ureq::Error>>) -> ureq::Response {
    result.unwrap_or_else(|err| match *err {
        ureq::Error::Status(status, resp) => {
            let body = resp
                .into_string()
                .unwrap_or_else(|e| format!("<unreadable: {e}>"));
            panic!("upstream answered {status}: {body:?}");
        }
        ureq::Error::Transport(t) => panic!("transport, never reached the proxy: {t}"),
    })
}

/// A stand-in for the Python API: echoes what it was asked, so the test can
/// assert on what actually crossed the hop.
async fn upstream_server() -> (String, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let app = Router::new()
        .route(
            "/api/legacy",
            get(move |headers: axum::http::HeaderMap| {
                counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    let cookie = headers
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-")
                        .to_string();
                    ([("x-from", "python")], format!("legacy cookie={cookie}"))
                }
            }),
        )
        .route(
            "/api/timeline",
            post(|| async { ([("x-from", "python")], "a method recalld does not serve") }),
        )
        .route(
            "/api/echo",
            post(|body: String| async move { format!("got:{body}") }),
        )
        .route(
            "/api/teapot",
            get(|| async { (axum::http::StatusCode::IM_A_TEAPOT, "short and stout") }),
        )
        .route(
            "/api/query",
            get(|uri: axum::http::Uri| async move { uri.query().unwrap_or("none").to_string() }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), hits)
}

async fn recalld_with(upstream: Option<String>) -> String {
    recalld_with_frontend(upstream, None).await
}

/// recalld with the browsing plane MOUNTED, which is how it runs on the fleet.
/// Without a gate configured the ported routes do not exist at all, so a test of
/// how they interact with the proxy would be testing an empty router.
async fn recalld_gated(upstream: Option<String>) -> String {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    std::mem::forget(dir);
    recalld::store::open(&root).expect("ingest db");
    let config = Arc::new(Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        webauth: Some(recalld::webauth::GateState {
            cfg: Arc::new(recalld::webauth::Config {
                session_secret: "test-secret-not-a-real-one".into(),
                client_id: "cid".into(),
                client_secret: "csec".into(),
                nc_base_url: "https://dash.example.org".into(),
                nc_internal_url: "https://dash.example.org".into(),
                redirect_uri: "http://127.0.0.1/auth/callback".into(),
                allowed_users: std::collections::HashSet::new(),
                device_token: None,
            }),
            now: Arc::new(|| 1_788_000_000),
        }),
        sync_token: None,
        upstream: upstream.map(|base| Upstream { base }),
        frontend: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recalld");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(config)).await;
    });
    format!("http://{addr}")
}

async fn recalld_with_frontend(
    upstream: Option<String>,
    frontend: Option<std::path::PathBuf>,
) -> String {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    std::mem::forget(dir); // the server outlives this fn; the tmpdir must too
    recalld::store::open(&root).expect("ingest db");
    let config = Arc::new(Config {
        root,
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        webauth: None,
        sync_token: None,
        upstream: upstream.map(|base| Upstream { base }),
        frontend,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recalld");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router(config)).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn an_unported_route_is_answered_by_the_upstream() {
    let (up, hits) = upstream_server().await;
    let base = recalld_with(Some(up)).await;

    let resp = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{base}/api/legacy"))
            .set("Cookie", "recall_session=abc.def")
            .call()
            .map_err(Box::new)
    })
    .await
    .expect("task");
    let resp = answered(resp);

    assert_eq!(resp.status(), 200);
    // ⚠ The session cookie must cross the hop verbatim, or Python cannot tell who
    // is asking and every proxied route 401s while the ported ones work — a split
    // brain that would look like a webauth bug.
    assert_eq!(
        resp.into_string().expect("body"),
        "legacy cookie=recall_session=abc.def"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_route_recalld_serves_is_never_proxied() {
    // ⚠ The safety property. `/ingest/v1/health` is recalld's own; if the fallback
    // could shadow it, a ported group would keep silently answering from Python
    // and the migration would appear to work while changing nothing.
    let (up, hits) = upstream_server().await;
    let base = recalld_with(Some(up)).await;

    let resp = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{base}/ingest/v1/health"))
            .call()
            .map_err(Box::new)
    })
    .await
    .expect("task");
    let resp = answered(resp);

    assert_eq!(resp.status(), 200);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "must not reach the upstream"
    );
}

#[tokio::test]
async fn the_upstreams_status_is_relayed_not_flattened() {
    // ureq treats 4xx/5xx as an error type. Reading that as "the proxy failed"
    // would turn every legitimate 404 from Python into a 502 from here, and a
    // missing session would read as a broken deployment.
    let (up, _) = upstream_server().await;
    let base = recalld_with(Some(up)).await;

    let status = tokio::task::spawn_blocking(move || {
        match ureq::get(&format!("{base}/api/teapot")).call() {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(e) => panic!("transport: {e}"),
        }
    })
    .await
    .expect("task");

    assert_eq!(status, 418);
}

#[tokio::test]
async fn a_request_body_and_query_survive_the_hop() {
    let (up, _) = upstream_server().await;
    let base = recalld_with(Some(up)).await;
    let b = base.clone();

    let echoed = tokio::task::spawn_blocking(move || {
        ureq::post(&format!("{b}/api/echo"))
            .send_string("hello")
            .expect("post")
            .into_string()
            .expect("body")
    })
    .await
    .expect("task");
    assert_eq!(echoed, "got:hello");

    let query = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{base}/api/query?limit=5&before=x"))
            .call()
            .expect("get")
            .into_string()
            .expect("body")
    })
    .await
    .expect("task");
    assert_eq!(query, "limit=5&before=x");
}

#[tokio::test]
async fn without_an_upstream_a_miss_is_an_honest_404() {
    // The default everywhere except the fleet: no upstream configured, so an
    // unknown route is absent rather than silently forwarded somewhere.
    let base = recalld_with(None).await;

    let status = tokio::task::spawn_blocking(move || {
        match ureq::get(&format!("{base}/api/legacy")).call() {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(e) => panic!("transport: {e}"),
        }
    })
    .await
    .expect("task");

    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_dead_upstream_is_a_502_not_an_empty_success() {
    // ⚠ During the migration this distinction is the whole diagnosis: "the Python
    // half is down" versus "that route legitimately has nothing". An empty 200
    // would send someone hunting a data bug that does not exist.
    let base = recalld_with(Some("http://127.0.0.1:1".to_string())).await;

    let status = tokio::task::spawn_blocking(move || {
        match ureq::get(&format!("{base}/api/legacy")).call() {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(e) => panic!("transport: {e}"),
        }
    })
    .await
    .expect("task");

    assert_eq!(status, 502);
}

#[test]
fn hop_by_hop_headers_do_not_cross() {
    // Host is the one that bites: forwarded, it makes the upstream answer for
    // recalld's name, which breaks any host-based routing in front of it.
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("host", "recall.xinutec.org".parse().expect("host"));
    headers.insert("cookie", "recall_session=x".parse().expect("cookie"));
    headers.insert("connection", "keep-alive".parse().expect("conn"));
    headers.insert("content-length", "17".parse().expect("len"));

    let sent = forwarded_headers(&headers);
    let names: Vec<_> = sent.iter().map(|(n, _)| n.as_str()).collect();

    assert_eq!(names, vec!["cookie"]);
}

#[test]
fn the_target_url_keeps_the_path_and_query_verbatim() {
    assert_eq!(
        target("http://127.0.0.1:8002", "/api/timeline", Some("limit=5")),
        "http://127.0.0.1:8002/api/timeline?limit=5"
    );
    assert_eq!(
        target("http://127.0.0.1:8002/", "/api/x", None),
        "http://127.0.0.1:8002/api/x"
    );
}

#[tokio::test]
async fn a_sync_path_is_proxied_and_never_answered_with_the_app_shell() {
    // ⚠ THE REGRESSION THAT BROKE THE FLEET, 2026-09-07. The fallback sent
    // `/api/*` to the proxy and EVERYTHING ELSE to the shell — but Python owns
    // `/sync/*` too, so the Mac's sync and jobs agents received index.html with
    // a 200 and died on `JSONDecodeError: Expecting value: line 1 column 1`.
    // The Mac could not push its archive or pull uploaded sessions, and the
    // status said success.
    //
    // Serving a frontend here is what makes this testable at all: with no
    // frontend configured everything falls through to the proxy and the bug
    // cannot reproduce, which is exactly why it reached production.
    let (up, hits) = upstream_server().await;
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><html></html>",
    )
    .expect("shell");
    let base = recalld_with_frontend(Some(up), Some(dir.path().to_path_buf())).await;

    let body = tokio::task::spawn_blocking(move || {
        ureq::get(&format!("{base}/sync/legacy"))
            .call()
            .map_err(Box::new)
    })
    .await
    .expect("task");

    // The upstream has no /sync/legacy, so it answers 404 — an HONEST miss.
    // Before the fix this was a 200 carrying HTML.
    match body {
        Err(boxed) => match *boxed {
            ureq::Error::Status(code, _) => assert_eq!(code, 404),
            ureq::Error::Transport(e) => panic!("transport: {e}"),
        },
        Ok(r) => panic!("expected a proxied 404, got {} ", r.status()),
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "no /api hit; it went to /sync"
    );
}

#[tokio::test]
async fn an_app_route_still_renders_the_shell() {
    // The other half of the same rule: a deep link is the frontend's, not the
    // upstream's, or every bookmarked session 404s.
    let (up, _) = upstream_server().await;
    let dir = tempfile::tempdir().expect("tmp");
    std::fs::write(
        dir.path().join("index.html"),
        "<!doctype html><html></html>",
    )
    .expect("shell");
    let base = recalld_with_frontend(Some(up), Some(dir.path().to_path_buf())).await;

    let body = tokio::task::spawn_blocking(move || {
        answered(
            ureq::get(&format!("{base}/sessions/meeting-x"))
                .call()
                .map_err(Box::new),
        )
        .into_string()
        .expect("body")
    })
    .await
    .expect("task");

    assert!(body.starts_with("<!doctype html>"), "got {body}");
}

/// ⚠ **A half-ported PATH is the trap this guards.** axum matches on the path
/// first: with `GET /api/sessions` mounted and no POST, a POST to it is answered
/// 405 by recalld and NEVER reaches the fallback — so the upload would have
/// stopped working the moment the list was ported, with the proxy sitting right
/// there. `method_not_allowed_fallback` is what makes owning half a path safe.
///
/// This runs with the browsing plane MOUNTED, because that is the only shape
/// where the collision exists. Every earlier proxy test ran with `webauth: None`
/// and could not have caught it — the same blind spot that let `/sync/*` reach
/// the fleet.
#[tokio::test]
async fn a_method_recalld_does_not_serve_falls_through_to_the_upstream() {
    let (upstream, _hits) = upstream_server().await;
    let base = recalld_gated(Some(upstream)).await;

    // ⚠ `/api/timeline` is recalld's GET and always will be, so a POST to it is
    // permanently a method miss. This deliberately does NOT use a route that is
    // mid-port: the first version used POST /api/sessions, which stopped being a
    // miss the day the upload moved, and the test failed for a reason that had
    // nothing to do with what it checks.
    let resp = tokio::task::spawn_blocking(move || {
        ureq::post(&format!("{base}/api/timeline"))
            .send_string("{}")
            .map_err(Box::new)
    })
    .await
    .expect("task");
    let resp = answered(resp);

    assert_eq!(resp.status(), 200, "not a 405 from recalld");
    assert_eq!(
        resp.header("x-from"),
        Some("python"),
        "and it must be the UPSTREAM that answered"
    );
}

#[tokio::test]
async fn a_method_miss_is_refused_when_there_is_nothing_to_fall_through_to() {
    // With no upstream the request is refused rather than answered, which is the
    // point. It is a 401 and not a 405 because the gate runs first here: with
    // nothing to forward to, "you are not signed in" is decided before "that
    // method is not mounted". Either is an honest refusal; what matters is that
    // no unported write is quietly accepted.
    let base = recalld_gated(None).await;

    let code = tokio::task::spawn_blocking(move || {
        match ureq::post(&format!("{base}/api/timeline")).send_string("{}") {
            Ok(resp) => resp.status(),
            Err(ureq::Error::Status(code, _)) => code,
            Err(other) => panic!("transport: {other}"),
        }
    })
    .await
    .expect("task");

    assert_eq!(code, 401);
}

/// ⚠ A proxied method-miss does NOT pass recalld's gate — it is handed to the
/// upstream unauthenticated, and PYTHON's gate is what refuses it. That is the
/// documented split (both halves validate the same cookie with the same secret),
/// but it is worth pinning: if Python's gate were ever removed on the assumption
/// that recalld already checked, this path would be open.
#[tokio::test]
async fn a_proxied_method_miss_is_gated_by_the_upstream_not_by_recalld() {
    let (upstream, _hits) = upstream_server().await;
    let base = recalld_gated(Some(upstream)).await;

    let resp = tokio::task::spawn_blocking(move || {
        ureq::post(&format!("{base}/api/timeline"))
            .send_string("{}")
            .map_err(Box::new)
    })
    .await
    .expect("task");
    let resp = answered(resp);

    // The stub upstream has no gate, so it answers — which is exactly the point:
    // recalld did not check, so the upstream must.
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.header("x-from"), Some("python"));
}
