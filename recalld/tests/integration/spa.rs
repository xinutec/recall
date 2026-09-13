//! Serving the built app. Each test is one of the three rules the handler exists
//! for; two of them were bought by real incidents.

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::routing::get;
use recalld::spa::{Frontend, resolve, serve};
use std::sync::Arc;
use tower::ServiceExt;

fn built(dir: &std::path::Path) -> Arc<Frontend> {
    std::fs::write(dir.join("index.html"), "<!doctype html><app-root>").expect("index");
    std::fs::write(dir.join("main-abc123.js"), "console.log(1)").expect("bundle");
    Arc::new(Frontend {
        root: dir.to_path_buf(),
    })
}

fn app(fe: Arc<Frontend>) -> Router {
    Router::new()
        .route("/api/timeline", get(|| async { "real route" }))
        .fallback(serve)
        .with_state(fe)
}

async fn get_path(app: &Router, path: &str) -> (u16, String, String) {
    let resp = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .expect("call");
    let status = resp.status().as_u16();
    let cache = resp
        .headers()
        .get("cache-control")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, cache, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn a_client_route_gets_the_shell_but_an_api_miss_gets_a_404() {
    // Rule 1. Returning the shell with 200 for an unknown /api/ path turns "this
    // route does not exist" into "here is some HTML", and the client's JSON parse
    // fails a long way from the cause.
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app(built(dir.path()));

    let (status, _, body) = get_path(&app, "/sessions/42").await;
    assert_eq!(status, 200, "a client-side route must get the shell");
    assert!(body.contains("app-root"));

    let (status, _, _) = get_path(&app, "/api/does-not-exist").await;
    assert_eq!(status, 404, "an API miss must look like a miss");

    // The real API route still wins over the fallback.
    let (status, _, body) = get_path(&app, "/api/timeline").await;
    assert_eq!(status, 200);
    assert_eq!(body, "real route");
}

#[tokio::test]
async fn the_shell_is_never_cached_and_the_hashed_bundle_is_cached_hard() {
    // Rule 2, and the reason it is a rule: index.html names the CURRENT bundles,
    // so caching it means a deploy is not picked up until a hard refresh. That is
    // the bug that served stale code from isis.
    let dir = tempfile::tempdir().expect("tempdir");
    let app = app(built(dir.path()));

    let (_, cache, _) = get_path(&app, "/").await;
    assert_eq!(cache, "no-cache", "the shell must never be cached");

    let (status, cache, _) = get_path(&app, "/main-abc123.js").await;
    assert_eq!(status, 200);
    assert!(
        cache.contains("immutable"),
        "hashed bundles are immutable: {cache}"
    );
}

#[tokio::test]
async fn a_request_cannot_escape_the_frontend_directory() {
    // Rule 3. Above the built app sit the archive, the database and the token
    // file; a traversal that reached any of them would be the whole ballgame.
    let outer = tempfile::tempdir().expect("tempdir");
    let secret = outer.path().join("tokens");
    std::fs::write(&secret, "usb super-secret-token").expect("secret");
    let dist = outer.path().join("dist");
    std::fs::create_dir(&dist).expect("dist");
    let fe = built(&dist);

    // Directly, and through the handler.
    assert_eq!(resolve(&fe.root, "../tokens"), None);
    assert_eq!(resolve(&fe.root, "/../tokens"), None);
    assert_eq!(resolve(&fe.root, "a/../../tokens"), None);

    let app = app(fe);
    for path in ["/../tokens", "/a/../../tokens", "/%2e%2e/tokens"] {
        let (status, _, body) = get_path(&app, path).await;
        assert!(
            !body.contains("super-secret-token"),
            "{path} escaped the frontend root"
        );
        // Falling back to the shell is fine; leaking the file is not.
        assert!(status == 200 || status == 404, "{path} -> {status}");
    }
}

#[tokio::test]
async fn an_unbuilt_frontend_says_so_rather_than_pretending() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fe = Arc::new(Frontend {
        root: dir.path().to_path_buf(),
    });
    let (status, _, body) = get_path(&app(fe), "/").await;
    assert_eq!(status, 404);
    assert!(body.contains("not built"), "{body}");
}
