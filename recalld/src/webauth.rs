//! Nextcloud SSO for the web UI: a sign-in plus a username allowlist. Inert
//! unless configured.
//!
//! Sessions are stateless `<payload>.<mac>` cookies signed with
//! `RECALL_SESSION_SECRET`; changing the format signs everyone out.
//!
//! - `/api/*` requires a session (401 without).
//! - A closed set of paths headless devices use stays open: they cannot sign
//!   in.
//! - A closed set accepts a device bearer instead of a cookie.

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashSet;

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "recall_session";
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;
const STATE_TTL_SECS: i64 = 10 * 60;

/// Paths open to devices that cannot sign in. The outbox report and heartbeat
/// carry no credential, so a phone with a wrong token can still say so. Anyone
/// on `WireGuard` or the LAN can fake a queue depth or a beat; neither grants
/// a read.
const DEVICE_EXEMPT: &[(&str, &str)] = &[
    ("GET", "/api/capture"),
    ("GET", "/api/sources"),
    ("POST", "/api/capture/pause"),
    ("POST", "/api/capture/resume"),
    ("POST", "/api/log"),
    ("POST", "/api/devices/outbox"),
    ("POST", "/api/devices/heartbeat"),
];

/// Where a `RECALL_DEVICE_TOKEN` bearer may stand in for a cookie: what the
/// phone does (upload), never a read.
const DEVICE_TOKEN_PATHS: &[(&str, &str)] = &[("POST", "/api/sessions")];

/// Who is signed in, carried entirely in the cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub user_id: String,
    pub display_name: String,
}

/// Lets `verify` enforce expiry, so no caller can forget it.
trait Expires {
    fn exp(&self) -> i64;
}

/// Field order is the serialised key order: keep it alphabetical, or the
/// token format changes.
#[derive(Serialize, Deserialize)]
struct SessionClaims {
    exp: i64,
    name: String,
    uid: String,
}

impl Expires for SessionClaims {
    fn exp(&self) -> i64 {
        self.exp
    }
}

#[derive(Serialize, Deserialize)]
struct StateClaims {
    exp: i64,
    rt: String,
}

impl Expires for StateClaims {
    fn exp(&self) -> i64 {
        self.exp
    }
}

/// Whether this request needs a session: `/api/*`, less the device-exempt
/// paths. Static assets, the OAuth routes and `/sync/*` are not gated.
#[must_use]
pub fn requires_session(method: &str, path: &str) -> bool {
    if !path.starts_with("/api/") {
        return false;
    }
    let m = method.to_ascii_uppercase();
    !DEVICE_EXEMPT.iter().any(|(em, ep)| *em == m && *ep == path)
}

#[must_use]
pub fn accepts_device_token(method: &str, path: &str) -> bool {
    let m = method.to_ascii_uppercase();
    DEVICE_TOKEN_PATHS
        .iter()
        .any(|(dm, dp)| *dm == m && *dp == path)
}

/// Who asked for a capture-control action, which needs no login: the signed-in
/// user if a valid cookie is present, else the device token, else the peer.
/// Never fails: annotating a pause must not break it.
#[must_use]
pub fn request_origin(
    cfg: Option<&Config>,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    authorization: Option<&str>,
    now: i64,
    client_host: Option<&str>,
) -> String {
    let host = client_host.unwrap_or("unknown-host");
    let Some(cfg) = cfg else {
        return format!("no-auth {host}");
    };
    if let Some(session) = read_session_cookie(&cfg.session_secret, cookie, now) {
        return format!("user {} {host}", session.user_id);
    }
    if cfg.presents_device_token(method, path, authorization) {
        return format!("device-token {host}");
    }
    format!("anon {host}")
}

/// A local redirect target: a single-slash absolute path, else `/`, so
/// `?return_to=` cannot make sign-in an open redirect.
pub fn validate_return_to(raw: Option<&str>) -> String {
    match raw {
        Some(r) if r.starts_with('/') && !r.starts_with("//") => r.to_owned(),
        _ => "/".to_owned(),
    }
}

fn b64e(raw: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn b64d(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .ok()
}

/// `<payload>.<mac>`: base64url JSON and its HMAC-SHA256 under the secret.
fn sign<T: Serialize>(secret: &str, claims: &T) -> Option<String> {
    let json = serde_json::to_string(claims).ok()?;
    let encoded = b64e(json.as_bytes());
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(encoded.as_bytes());
    Some(format!("{encoded}.{}", b64e(&mac.finalize().into_bytes())))
}

/// The payload if the MAC checks out (in constant time) and the token has not
/// expired. A forged cookie looks the same as a corrupt one.
fn verify<T: for<'de> Deserialize<'de> + Expires>(
    secret: &str,
    token: &str,
    now: i64,
) -> Option<T> {
    let (encoded, presented) = token.split_once('.')?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(encoded.as_bytes());
    mac.verify_slice(&b64d(presented)?).ok()?;
    let raw = b64d(encoded)?;
    let claims: T = serde_json::from_slice(&raw).ok()?;
    if claims.exp() < now {
        return None;
    }
    Some(claims)
}

#[must_use]
pub fn make_session_cookie(secret: &str, session: &Session, now: i64) -> Option<String> {
    sign(
        secret,
        &SessionClaims {
            exp: now + SESSION_TTL_SECS,
            name: session.display_name.clone(),
            uid: session.user_id.clone(),
        },
    )
}

#[must_use]
pub fn read_session_cookie(secret: &str, token: Option<&str>, now: i64) -> Option<Session> {
    let claims: SessionClaims = verify(secret, token?, now)?;
    Some(Session {
        user_id: claims.uid,
        display_name: claims.name,
    })
}

#[must_use]
pub fn make_state(secret: &str, return_to: Option<&str>, now: i64) -> Option<String> {
    sign(
        secret,
        &StateClaims {
            exp: now + STATE_TTL_SECS,
            rt: validate_return_to(return_to),
        },
    )
}

/// The `return_to` of an authentic, fresh state token.
#[must_use]
pub fn read_state(secret: &str, token: &str, now: i64) -> Option<String> {
    let claims: StateClaims = verify(secret, token, now)?;
    Some(validate_return_to(Some(&claims.rt)))
}

/// The gate's configuration. Absent: an open LAN UI, for dev and tests.
#[derive(Debug, Clone)]
pub struct Config {
    pub session_secret: String,
    pub client_id: String,
    pub client_secret: String,
    pub nc_base_url: String,
    /// Where server-to-server calls go; the fleet uses the cluster-local
    /// service.
    pub nc_internal_url: String,
    pub redirect_uri: String,
    /// Empty admits any Nextcloud user. Defaults to one.
    pub allowed_users: HashSet<String>,
    pub device_token: Option<String>,
}

impl Config {
    /// [`Self::from_env`] over the process environment, each key read by its
    /// literal name so the deploy-env contract sees it.
    #[must_use]
    pub fn from_process_env() -> Option<Self> {
        let read = [
            (
                "RECALL_SESSION_SECRET",
                std::env::var("RECALL_SESSION_SECRET").ok(),
            ),
            ("NC_CLIENT_ID", std::env::var("NC_CLIENT_ID").ok()),
            ("NC_CLIENT_SECRET", std::env::var("NC_CLIENT_SECRET").ok()),
            ("NC_BASE_URL", std::env::var("NC_BASE_URL").ok()),
            ("NC_INTERNAL_URL", std::env::var("NC_INTERNAL_URL").ok()),
            ("NC_REDIRECT_URI", std::env::var("NC_REDIRECT_URI").ok()),
            (
                "RECALL_ALLOWED_USERS",
                std::env::var("RECALL_ALLOWED_USERS").ok(),
            ),
            (
                "RECALL_DEVICE_TOKEN",
                std::env::var("RECALL_DEVICE_TOKEN").ok(),
            ),
        ];
        Self::from_env(&|key| {
            let hit = read.iter().find(|(name, _)| *name == key);
            debug_assert!(
                hit.is_some(),
                "from_env asks for {key}, which is not read above"
            );
            hit.and_then(|(_, value)| value.clone())
        })
    }

    /// On only with the whole OAuth triple: half a gate would refuse everyone.
    #[must_use]
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Option<Self> {
        let session_secret = get("RECALL_SESSION_SECRET")?;
        let client_id = get("NC_CLIENT_ID")?;
        let client_secret = get("NC_CLIENT_SECRET")?;
        if session_secret.is_empty() || client_id.is_empty() || client_secret.is_empty() {
            return None;
        }
        Some(Self {
            session_secret,
            client_id,
            client_secret,
            nc_base_url: get("NC_BASE_URL")
                .unwrap_or_else(|| "https://dash.xinutec.org".to_owned()),
            nc_internal_url: get("NC_INTERNAL_URL")
                .map(|u| u.trim_end_matches('/').to_owned())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| {
                    get("NC_BASE_URL").unwrap_or_else(|| "https://dash.xinutec.org".to_owned())
                }),
            redirect_uri: get("NC_REDIRECT_URI")
                .unwrap_or_else(|| "https://recall.xinutec.org/auth/callback".to_owned()),
            allowed_users: get("RECALL_ALLOWED_USERS")
                // dev-lint: allow-pii one account by default: recall holds private audio
                .unwrap_or_else(|| "pippijn".to_owned())
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            device_token: get("RECALL_DEVICE_TOKEN").filter(|t| !t.is_empty()),
        })
    }

    #[must_use]
    pub fn permits(&self, user_id: &str) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.contains(user_id)
    }

    #[must_use]
    pub fn presents_device_token(
        &self,
        method: &str,
        path: &str,
        authorization: Option<&str>,
    ) -> bool {
        let Some(expected) = self.device_token.as_deref() else {
            return false;
        };
        if !accepts_device_token(method, path) {
            return false;
        }
        let Some(presented) = authorization.and_then(|a| a.strip_prefix("Bearer ")) else {
            return false;
        };
        // HMACs, not `==` on the tokens, which leaks a prefix through timing.
        let Ok(mut mac) = HmacSha256::new_from_slice(self.session_secret.as_bytes()) else {
            return false;
        };
        mac.update(presented.as_bytes());
        let Ok(mut expect_mac) = HmacSha256::new_from_slice(self.session_secret.as_bytes()) else {
            return false;
        };
        expect_mac.update(expected.as_bytes());
        mac.finalize().into_bytes() == expect_mac.finalize().into_bytes()
    }
}

#[must_use]
pub fn authorize_url(cfg: &Config, state: &str) -> String {
    let q = form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", &cfg.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &cfg.redirect_uri)
        .append_pair("state", state)
        .finish();
    format!("{}/index.php/apps/oauth2/authorize?{q}", cfg.nc_base_url)
}

/// The URL and `Host` header for a server-side Nextcloud call: to the internal
/// address, presenting the public host for Nextcloud's trusted-domain check.
#[must_use]
pub fn server_call(cfg: &Config, path: &str) -> (String, Option<String>) {
    let url = format!("{}{path}", cfg.nc_internal_url);
    if cfg.nc_internal_url == cfg.nc_base_url {
        return (url, None);
    }
    let host = cfg
        .nc_base_url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .map(str::to_owned);
    (url, host)
}

/// Every variant answers the same 502: a visitor does not learn which half of
/// the exchange failed.
#[derive(Debug)]
pub enum AuthError {
    Transport(String),
    Malformed(&'static str),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(e) => write!(f, "nextcloud call failed: {e}"),
            Self::Malformed(what) => write!(f, "nextcloud response {what}"),
        }
    }
}

/// A transport failure, with each io error's kind and errno in the chain: the
/// message alone does not say which layer failed.
fn transport(e: &ureq::Error) -> AuthError {
    use std::fmt::Write;
    let mut detail = e.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = source {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            let _ = write!(detail, " [io {} os={:?}]", io.kind(), io.raw_os_error());
        }
        source = err.source();
    }
    AuthError::Transport(detail)
}

/// Not `ureq::get`'s shared pool: a pooled socket the far end closed fails the
/// next call with `os error 22`. No reuse costs nothing for two calls.
fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .max_idle_connections(0)
        .timeout(std::time::Duration::from_secs(15))
        .build()
}

pub fn exchange_code(cfg: &Config, code: &str) -> Result<String, AuthError> {
    let (url, host) = server_call(cfg, "/index.php/apps/oauth2/api/v1/token");
    let mut req = agent().post(&url);
    if let Some(h) = host.as_deref() {
        req = req.set("Host", h);
    }
    let resp: serde_json::Value = req
        .send_form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &cfg.client_id),
            ("client_secret", &cfg.client_secret),
            ("redirect_uri", &cfg.redirect_uri),
        ])
        .map_err(|e| transport(&e))?
        .into_json()
        .map_err(|e| {
            AuthError::Transport(format!("{e} [io {} os={:?}]", e.kind(), e.raw_os_error()))
        })?;
    match resp.get("access_token").and_then(serde_json::Value::as_str) {
        Some(t) if !t.is_empty() => Ok(t.to_owned()),
        _ => Err(AuthError::Malformed("missing access_token")),
    }
}

/// Who signed in, via the OCS user endpoint. The access token is used once and
/// dropped.
pub fn fetch_userinfo(cfg: &Config, access_token: &str) -> Result<Session, AuthError> {
    let (url, host) = server_call(cfg, "/ocs/v2.php/cloud/user?format=json");
    let mut req = agent()
        .get(&url)
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("OCS-APIRequest", "true");
    if let Some(h) = host.as_deref() {
        req = req.set("Host", h);
    }
    let resp: serde_json::Value =
        req.call()
            .map_err(|e| transport(&e))?
            .into_json()
            .map_err(|e| {
                AuthError::Transport(format!("{e} [io {} os={:?}]", e.kind(), e.raw_os_error()))
            })?;
    let data = resp.get("ocs").and_then(|o| o.get("data"));
    let uid = data
        .and_then(|d| d.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(AuthError::Malformed("missing id"))?;
    // A missing display name falls back to the id rather than failing sign-in.
    let name = data
        .and_then(|d| d.get("displayname"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(uid);
    Ok(Session {
        user_id: uid.to_owned(),
        display_name: name.to_owned(),
    })
}

// --- the HTTP surface ----------------------------------------------------------

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::sync::Arc;

/// Injected, for tests.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Clone)]
pub struct GateState {
    pub cfg: Arc<Config>,
    pub now: Clock,
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then(|| v.to_owned())
        })
}

/// The gate on every request; it decides and never serves. The device token is
/// tried only after the cookie fails.
pub async fn gate(
    State(st): State<GateState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    if !requires_session(&method, &path) {
        return next.run(request).await;
    }
    let headers = request.headers().clone();
    let now = (st.now)();
    let cookie = cookie_value(&headers, COOKIE_NAME);
    let Some(session) = read_session_cookie(&st.cfg.session_secret, cookie.as_deref(), now) else {
        let auth = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        if st.cfg.presents_device_token(&method, &path, auth) {
            return next.run(request).await;
        }
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "not authenticated"})),
        )
            .into_response();
    };
    if !st.cfg.permits(&session.user_id) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "not authorised"})),
        )
            .into_response();
    }
    next.run(request).await
}

#[derive(Deserialize)]
pub struct LoginQuery {
    return_to: Option<String>,
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
}

/// Shared by login and logout. `Secure`: recall is served only over https
/// (#1799); browsers treat `http://localhost` as secure, so dev still works.
const COOKIE_ATTRS: &str = "Path=/; HttpOnly; Secure; SameSite=Lax";

fn set_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; Max-Age={SESSION_TTL_SECS}; {COOKIE_ATTRS}")
}

/// The OAuth flow (`/login`, `/auth/callback`, `/logout`) and `/api/me`.
pub fn routes(st: GateState) -> Router {
    Router::new()
        .route(
            "/login",
            get(
                |State(st): State<GateState>, Query(q): Query<LoginQuery>| async move {
                    match make_state(&st.cfg.session_secret, q.return_to.as_deref(), (st.now)()) {
                        Some(state) => Redirect(authorize_url(&st.cfg, &state)).into_response(),
                        None => sign_in_problem(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "The sign-in could not be started.",
                        ),
                    }
                },
            ),
        )
        .route("/auth/callback", get(callback))
        .route(
            "/logout",
            post(|| async {
                (
                    [(
                        header::SET_COOKIE,
                        format!("{COOKIE_NAME}=; Max-Age=0; {COOKIE_ATTRS}"),
                    )],
                    Redirect("/".to_owned()),
                )
                    .into_response()
            }),
        )
        .route("/api/me", get(me))
        .with_state(st)
}

/// A sign-in that could not be finished, drawn for the browser: these routes are
/// where a browser is sent, and JSON there reads as the app being broken.
fn sign_in_problem(status: StatusCode, said: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Sign-in did not finish</title><style>\
         body{{font:16px/1.5 system-ui,-apple-system,sans-serif;margin:0;\
         min-height:100vh;display:grid;place-items:center;padding:1.5rem;color:#1a1a1a}}\
         main{{max-width:26rem}}h1{{font-size:1.2rem;margin:0 0 .5rem}}\
         p{{margin:0 0 1.5rem;color:#555}}\
         a{{display:inline-block;padding:.65rem 1.1rem;border-radius:.5rem;\
         background:#1b6ac9;color:#fff;text-decoration:none}}\
         </style></head><body><main><h1>Sign-in did not finish</h1>\
         <p>{said}</p><a href=\"/login\">Try again</a></main></body></html>"
    );
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

/// A 302.
struct Redirect(String);

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        (StatusCode::FOUND, [(header::LOCATION, self.0)]).into_response()
    }
}

async fn callback(State(st): State<GateState>, Query(q): Query<CallbackQuery>) -> Response {
    let now = (st.now)();
    // State first, so a stranger cannot make this server call Nextcloud.
    let Some(return_to) = q
        .state
        .as_deref()
        .and_then(|s| read_state(&st.cfg.session_secret, s, now))
    else {
        return sign_in_problem(
            StatusCode::FORBIDDEN,
            "This sign-in did not start here, or it took too long.",
        );
    };
    let Some(code) = q.code.filter(|c| !c.is_empty()) else {
        return sign_in_problem(
            StatusCode::BAD_REQUEST,
            "Nextcloud sent no authorization code.",
        );
    };
    let cfg = Arc::clone(&st.cfg);
    let resolved = tokio::task::spawn_blocking(move || {
        exchange_code(&cfg, &code).and_then(|t| fetch_userinfo(&cfg, &t))
    })
    .await;
    let session = match resolved {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!("nextcloud sign-in failed: {e}");
            return sign_in_problem(
                StatusCode::BAD_GATEWAY,
                "The sign-in could not be finished.",
            );
        }
        Err(e) => {
            tracing::warn!("sign-in task failed: {e}");
            return sign_in_problem(
                StatusCode::BAD_GATEWAY,
                "The sign-in could not be finished.",
            );
        }
    };
    if !st.cfg.permits(&session.user_id) {
        return sign_in_problem(StatusCode::FORBIDDEN, "This account may not use this app.");
    }
    let Some(token) = make_session_cookie(&st.cfg.session_secret, &session, now) else {
        return sign_in_problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The sign-in could not be finished.",
        );
    };
    (
        [(header::SET_COOKIE, set_cookie(&token))],
        Redirect(return_to),
    )
        .into_response()
}

/// Who is signed in: the SPA's login probe.
async fn me(State(st): State<GateState>, headers: HeaderMap) -> Response {
    match read_session_cookie(
        &st.cfg.session_secret,
        cookie_value(&headers, COOKIE_NAME).as_deref(),
        (st.now)(),
    ) {
        Some(s) => Json(serde_json::json!({
            "userId": s.user_id, "displayName": s.display_name
        }))
        .into_response(),
        None => StatusCode::UNAUTHORIZED.into_response(),
    }
}
