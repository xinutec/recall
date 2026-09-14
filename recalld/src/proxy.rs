//! The strangler fallback: forward anything recalld does not serve yet to the
//! Python API beside it in the pod.
//!
//! ⚠ **This exists so Python can be deleted a route group at a time.** Without it
//! the cutover is all-or-nothing: the browser talks to whichever host served the
//! page, so a route ported to recalld changes nothing until EVERY route has moved
//! and the app is re-pointed. That made "port a group, delete its module" — the
//! only way this migration gets finished — impossible. With recalld as the front
//! door and this as the fallback, each ported group deletes its Python the same
//! day, and what remains here at the end is one `None` and a deleted module.
//!
//! ⚠ **It is a FALLBACK, never an override.** It runs only where recalld's own
//! router had no match, so a ported route always wins. That ordering is the whole
//! safety property: a half-ported group cannot silently keep serving the old
//! answer, and a typo'd path cannot shadow a real handler.
//!
//! ⚠ **The upstream is loopback inside one pod**, not a network peer. Both
//! containers share a network namespace, so this hop never leaves the machine and
//! needs no TLS — which matters, because the workspace's `ureq` is deliberately
//! built without it.
//!
//! ⚠ **The session cookie is forwarded verbatim**, which is what makes the two
//! halves agree about who you are. recalld's gate has already authenticated the
//! request; Python re-validates the same `<payload>.<mac>` with the same secret
//! and reaches the same verdict. If the two ever stop agreeing, the golden-token
//! test in `webauth` fails first and says so plainly.

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Headers that describe THIS hop and must not be copied onto the next one.
/// Forwarding `Host` would make Python answer for recalld's name; forwarding a
/// transfer or connection header would describe a framing that no longer applies.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

fn droppable(name: &HeaderName) -> bool {
    HOP_BY_HOP.contains(&name.as_str())
}

/// Where unported routes go. `None` = no upstream, so a miss is an honest 404.
#[derive(Clone, Debug)]
pub struct Upstream {
    /// Base URL, e.g. `http://127.0.0.1:8002`.
    pub base: String,
}

/// The agent every forward uses.
///
/// ⚠ **NOT `ureq::request`, and that was a real 502 rather than a style point.**
/// The free functions use ureq's GLOBAL agent, whose connection pool is shared
/// process-wide. A pooled socket the upstream has already closed is handed back
/// on the next forward, and the failure lands where the body is read — the
/// proxy answers `502 upstream body failed` having reached a dead connection.
///
/// #1480 chased exactly this shape as an intermittent GATE failure and fixed it
/// in `tests/proxy.rs` on 2026-09-11, which made the tests stop flaking and left
/// the same bug in the code they test. It surfaced again the same day, under
/// load, in the workspace run — and said so precisely, because `answered`
/// distinguishes the three 502 bodies.
///
/// `max_idle_connections(0)` removes reuse entirely. That would be a real cost on
/// a hot path and is free here: measured 2026-09-11, the upstream this forwards
/// to serves nothing but its own liveness probe.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .max_idle_connections(0)
        .timeout_connect(std::time::Duration::from_secs(5))
        .build()
}

/// Build the upstream URL for a request: base + path + query, unchanged.
#[must_use]
pub fn target(base: &str, path: &str, query: Option<&str>) -> String {
    let mut url = format!("{}{}", base.trim_end_matches('/'), path);
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    url
}

/// Copy headers for the outbound hop, dropping the ones that describe this one.
#[must_use]
pub fn forwarded_headers(incoming: &HeaderMap) -> Vec<(String, String)> {
    incoming
        .iter()
        .filter(|(name, _)| !droppable(name))
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Copy the upstream's headers back to the caller, minus this-hop ones.
fn response_headers(resp: &ureq::Response) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in resp.headers_names() {
        let Ok(header) = HeaderName::try_from(name.as_str()) else {
            continue;
        };
        if droppable(&header) {
            continue;
        }
        if let Some(value) = resp.header(&name)
            && let Ok(v) = HeaderValue::from_str(value)
        {
            out.insert(header, v);
        }
    }
    out
}

/// Forward one request and return the upstream's answer.
///
/// ⚠ A dead upstream is a **502, never an empty 200**. During the migration this
/// is the difference between "the Python half is down" and "that route legitimately
/// has nothing", and reading the second for the first would send someone hunting a
/// data bug that does not exist.
pub async fn forward(up: Upstream, req: Request) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().map(str::to_string);
    let headers = forwarded_headers(req.headers());
    let body = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("proxy could not read the request body: {e}");
            return (StatusCode::BAD_REQUEST, "unreadable body").into_response();
        }
    };

    let url = target(&up.base, &path, query.as_deref());
    // ⚠ THE WHOLE EXCHANGE IS IN HERE, body included, and that placement is the
    // fix for a deadlock rather than tidiness. `ureq`'s reader is a blocking
    // socket read, so reading the body on the async side blocks a runtime
    // worker for as long as the upstream takes to write. On the fleet's
    // multi-thread runtime that costs one worker; in the tests, which run a
    // CURRENT-THREAD runtime with the stub upstream on it, it blocks the very
    // thread that has to drive the upstream's write — so the read can never
    // complete and the proxy answers `502 upstream body failed` once the socket
    // times out. That is the intermittent gate failure #1480 kept re-finding:
    // a small body is already buffered and reads instantly, so it only fires
    // when the machine is loaded enough for the write to still be in flight.
    let sent = tokio::task::spawn_blocking(move || {
        let mut req = agent().request(method.as_str(), &url);
        for (name, value) in headers {
            req = req.set(&name, &value);
        }
        match req.send_bytes(&body) {
            // ureq treats 4xx/5xx as Err(Status): that is the upstream
            // ANSWERING, so it takes the same path as a 200 — its answer belongs
            // to the caller unchanged, and a 404 from Python must not become a
            // 502 from here.
            Ok(resp) | Err(ureq::Error::Status(_, resp)) => read_fully(resp),
            Err(e) => Err(Failed::Unreachable(e.to_string())),
        }
    })
    .await;

    match sent {
        Ok(Ok(relayed)) => relayed.into_response(),
        Ok(Err(Failed::Unreachable(e))) => {
            tracing::warn!("proxy upstream unreachable: {e}");
            (StatusCode::BAD_GATEWAY, "upstream unreachable").into_response()
        }
        Ok(Err(Failed::Body(e))) => {
            tracing::warn!("proxy could not read the upstream body: {e}");
            (StatusCode::BAD_GATEWAY, "upstream body failed").into_response()
        }
        Err(e) => {
            tracing::warn!("proxy task failed: {e}");
            (StatusCode::BAD_GATEWAY, "proxy failed").into_response()
        }
    }
}

/// Why a forward produced no answer. The two are kept apart because they point
/// at different causes and the 502 bodies name them — see `tests/proxy.rs`,
/// where telling them apart is what turned an intermittent 502 into a diagnosis.
enum Failed {
    Unreachable(String),
    Body(String),
}

/// The upstream's answer, entirely read, so nothing about it still needs a
/// socket.
struct Relayed {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Relayed {
    fn into_response(self) -> Response {
        let mut out = (self.status, Body::from(Bytes::from(self.body))).into_response();
        *out.headers_mut() = self.headers;
        out
    }
}

/// Drain a `ureq` response. Called ONLY from the blocking side.
fn read_fully(resp: ureq::Response) -> Result<Relayed, Failed> {
    let status = StatusCode::from_u16(resp.status()).unwrap_or(StatusCode::BAD_GATEWAY);
    let headers = response_headers(&resp);
    let mut body = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut body)
        .map_err(|e| Failed::Body(format!("{} ({e})", e.kind())))?;
    Ok(Relayed {
        status,
        headers,
        body,
    })
}
