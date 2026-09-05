//! The recorder contract, end to end through the router: receipts a recorder
//! can trust its eviction to, idempotent redelivery, append-only conflicts,
//! and the two credential planes.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use recalld::app::{Config, router};
use recalld::tokens::Tokens;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;
use tower::util::ServiceExt;

struct Harness {
    app: Router,
    // Held for its lifetime: dropping it deletes the store under the app.
    dir: tempfile::TempDir,
}

fn harness(tokens: Option<&str>, read_token: Option<&str>) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let tokens = tokens.map(|text| {
        let path = dir.path().join("tokens");
        std::fs::write(&path, text).expect("write tokens");
        Tokens::load(&path).expect("load tokens")
    });
    let app = router(Arc::new(Config {
        root: dir.path().to_owned(),
        tokens,
        read_token: read_token.map(str::to_owned),
        max_body_bytes: 1024 * 1024,
    }));
    Harness { app, dir }
}

fn put(source: &str, filename: &str, body: &[u8], token: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("PUT")
        .uri(format!("/ingest/v1/segments/{source}/{filename}"));
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.body(Body::from(body.to_vec())).expect("request")
}

fn get(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut request = Request::builder().method("GET").uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    request.body(Body::empty()).expect("request")
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    // Every route behind `send` answers JSON; a body that does not parse is
    // a broken response, never "no data".
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("response body is JSON")
    };
    (status, json)
}

async fn send_raw(app: &Router, request: Request<Body>) -> (StatusCode, Vec<u8>) {
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, bytes.to_vec())
}

fn blob_path(dir: &Path, source: &str, filename: &str) -> std::path::PathBuf {
    dir.join("ingest").join(source).join(filename)
}

#[tokio::test]
async fn health_answers() {
    let h = harness(None, None);
    let (status, body) = send(&h.app, get("/ingest/v1/health", None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn the_receipt_is_the_hash_of_what_was_stored() {
    let h = harness(None, None);
    let audio = b"pretend flac bytes";
    let (status, body) = send(&h.app, put("usb", "usb-20260905T120000.flac", audio, None)).await;
    assert_eq!(status, StatusCode::OK);
    // The recorder's eviction rule: re-hash the local file, compare. So the
    // receipt must equal an independent sha-256 of the bytes sent.
    assert_eq!(body["sha256"], hex::encode(Sha256::digest(audio)));
    assert_eq!(body["bytes"], audio.len());
    let stored = std::fs::read(blob_path(h.dir.path(), "usb", "usb-20260905T120000.flac"))
        .expect("blob stored");
    assert_eq!(stored, audio);
}

#[tokio::test]
async fn redelivery_of_identical_bytes_is_idempotent() {
    let h = harness(None, None);
    let audio = b"same bytes";
    let first = send(&h.app, put("usb", "usb-20260905T120000.flac", audio, None)).await;
    let second = send(&h.app, put("usb", "usb-20260905T120000.flac", audio, None)).await;
    assert_eq!(first, second);
    assert_eq!(first.0, StatusCode::OK);
}

#[tokio::test]
async fn a_divergent_redelivery_is_refused_and_the_original_kept() {
    let h = harness(None, None);
    let original = b"the first delivery";
    send(
        &h.app,
        put("usb", "usb-20260905T120000.flac", original, None),
    )
    .await;
    let (status, _) = send(
        &h.app,
        put("usb", "usb-20260905T120000.flac", b"different bytes", None),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let stored = std::fs::read(blob_path(h.dir.path(), "usb", "usb-20260905T120000.flac"))
        .expect("blob kept");
    assert_eq!(stored, original);
}

#[tokio::test]
async fn a_bad_name_is_refused_before_anything_is_written() {
    let h = harness(None, None);
    for (source, filename) in [
        ("usb", "geb-20260905T120000.flac"),
        ("usb", "usb-20260905T120000.mp3"),
        ("usb", "usb-2026.flac"),
        ("USB", "USB-20260905T120000.flac"),
    ] {
        let (status, _) = send(&h.app, put(source, filename, b"x", None)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{source}/{filename}");
    }
    assert!(!h.dir.path().join("ingest").exists());
}

#[tokio::test]
async fn the_write_gate_distinguishes_unknown_from_misdirected() {
    let h = harness(Some("usb secret-a\npixel5 secret-b\n"), None);
    let name = "usb-20260905T120000.flac";
    let (none, _) = send(&h.app, put("usb", name, b"x", None)).await;
    assert_eq!(none, StatusCode::UNAUTHORIZED);
    let (wrong, _) = send(&h.app, put("usb", name, b"x", Some("nonsense"))).await;
    assert_eq!(wrong, StatusCode::UNAUTHORIZED);
    let (misdirected, _) = send(&h.app, put("usb", name, b"x", Some("secret-b"))).await;
    assert_eq!(misdirected, StatusCode::FORBIDDEN);
    let (right, _) = send(&h.app, put("usb", name, b"x", Some("secret-a"))).await;
    assert_eq!(right, StatusCode::OK);
}

#[tokio::test]
async fn unconfigured_gates_are_open() {
    // The inert-unless-configured pattern: dev and tests carry no ceremony.
    let h = harness(None, None);
    let (status, _) = send(&h.app, put("usb", "usb-20260905T120000.flac", b"x", None)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&h.app, get("/ingest/v1/segments", None)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn an_oversize_body_is_refused() {
    let h = harness(None, None);
    let oversize = vec![0u8; 2 * 1024 * 1024];
    // The limit layer answers before our handlers, with a plain-text body —
    // read the status raw rather than pretending it is our JSON.
    let (status, _) = send_raw(
        &h.app,
        put("usb", "usb-20260905T120000.flac", &oversize, None),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn the_listing_walks_time_forward_and_filters() {
    let h = harness(None, None);
    for (source, name) in [
        ("usb", "usb-20260905T120100.flac"),
        ("usb", "usb-20260905T120000.flac"),
        ("pixel5", "pixel5-20260905T120000.flac"),
    ] {
        let (status, _) = send(&h.app, put(source, name, b"x", None)).await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = send(&h.app, get("/ingest/v1/segments?source=usb", None)).await;
    assert_eq!(status, StatusCode::OK);
    let segments = body["segments"].as_array().expect("array");
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0]["filename"], "usb-20260905T120000.flac");
    assert_eq!(segments[1]["filename"], "usb-20260905T120100.flac");
    let (_, body) = send(
        &h.app,
        get(
            "/ingest/v1/segments?source=usb&since=2026-09-05T12:00:30Z",
            None,
        ),
    )
    .await;
    assert_eq!(body["segments"].as_array().expect("array").len(), 1);
}

#[tokio::test]
async fn the_read_side_is_the_sync_tokens_not_the_devices() {
    let h = harness(Some("usb secret-a\n"), Some("sync-token"));
    let name = "usb-20260905T120000.flac";
    let (status, _) = send(&h.app, put("usb", name, b"audio", Some("secret-a"))).await;
    assert_eq!(status, StatusCode::OK);
    // The device's own token cannot read — write-only is the plane's promise.
    for token in [None, Some("secret-a")] {
        let (status, _) = send(&h.app, get("/ingest/v1/segments", token)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = send(&h.app, get(&format!("/ingest/v1/blob/usb/{name}"), token)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, bytes) = send_raw(
        &h.app,
        get(&format!("/ingest/v1/blob/usb/{name}"), Some("sync-token")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"audio");
}

#[tokio::test]
async fn a_missing_blob_is_404() {
    let h = harness(None, None);
    let (status, _) = send(
        &h.app,
        get("/ingest/v1/blob/usb/usb-20260905T120000.flac", None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_blob_without_a_row_heals_on_redelivery() {
    // The crash window: rename landed, row insert did not. The next identical
    // PUT must repair the row and hand out the normal receipt.
    let h = harness(None, None);
    let audio = b"recovered bytes";
    let dir = h.dir.path().join("ingest").join("usb");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("usb-20260905T120000.flac"), audio).expect("orphan blob");
    let (status, body) = send(&h.app, put("usb", "usb-20260905T120000.flac", audio, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sha256"], hex::encode(Sha256::digest(audio)));
    let (_, listing) = send(&h.app, get("/ingest/v1/segments?source=usb", None)).await;
    assert_eq!(listing["segments"].as_array().expect("array").len(), 1);
}

// --- liveness for store-and-forward recorders (#1428) ------------------------

#[tokio::test]
async fn liveness_reports_each_source_newest_capture_time() {
    let h = harness(Some("* write\n"), Some("read"));
    // Two sources, delivered out of order — the NEWEST capture must win.
    for name in [
        "geb-20260905T100000.opus",
        "geb-20260905T100200.opus",
        "geb-20260905T100100.opus",
    ] {
        let (status, _) = send(&h.app, put("geb", name, b"a", Some("write"))).await;
        assert_eq!(status, StatusCode::OK, "{name}");
    }
    let (status, _) = send(
        &h.app,
        put("usb", "usb-20260905T090000.opus", b"b", Some("write")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&h.app, get("/ingest/v1/liveness", Some("read"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sources"]["geb"], "2026-09-05T10:02:00Z");
    assert_eq!(body["sources"]["usb"], "2026-09-05T09:00:00Z");
    let _ = &h.dir;
}

#[tokio::test]
async fn liveness_is_behind_the_read_plane_not_the_write_one() {
    let h = harness(Some("* write\n"), Some("read"));
    // A recorder's WRITE token must not open the read side.
    let (status, _) = send(&h.app, get("/ingest/v1/liveness", Some("write"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = send(&h.app, get("/ingest/v1/liveness", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let _ = &h.dir;
}
