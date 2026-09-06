//! Nextcloud SSO for the human-facing web UI (stage F1), ported from
//! `recall.webauth`. Inert unless configured.
//!
//! This is the gate that has to exist before recalld may serve a single
//! transcript. The browsing plane's promise is a Nextcloud sign-in plus a
//! username allowlist; recalld's own read side takes the sync token, which is
//! weaker. Mounting reads before this landed would have opened a second, weaker
//! door to the household's audio, so the read layer was deliberately left off the
//! router until this exists (docs/architecture.md, F1).
//!
//! ⚠ **The token format is deliberately IDENTICAL to the Python's**, and that is
//! the one place in this rebuild where compatibility is worth having. It is not
//! fidelity for its own sake: sharing `RECALL_SESSION_SECRET` and the exact
//! `<payload>.<mac>` shape means a cookie minted by the Python OAuth flow
//! verifies here, so recalld can be mounted behind the existing sign-in and
//! cut over route-group by route-group WITHOUT a second login or a flag day.
//! Change the format and the two halves stop recognising each other mid-migration.
//!
//! Three planes, as before:
//!
//! * **Browsing** — `/api/*` requires a session; a request without one gets 401.
//! * **Recording** — a closed set of paths a headless device uses stays open,
//!   because a device cannot perform an interactive OAuth login.
//! * **Device token** — a closed set where a bearer stands in for a cookie.

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::HashSet;

type HmacSha256 = Hmac<Sha256>;

pub const COOKIE_NAME: &str = "recall_session";
const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;
const STATE_TTL_SECS: i64 = 10 * 60;

/// Paths that stay open because the caller is a device or daemon that cannot sign
/// in interactively. Copied deliberately from `recall.webauth._DEVICE_EXEMPT` —
/// each entry there carries an argued reason, and two of them are subtle enough
/// to be worth restating:
///
/// ⚠ `POST /api/devices/outbox` reports what a phone could NOT upload, using the
/// same credential it uploads with. Gating it means that when the token is wrong
/// — the likeliest fault, and the one this check exists to catch — the report is
/// 401'd too and the fleet learns nothing.
///
/// ⚠ `POST /api/devices/heartbeat` carries no credential at all, because a phone
/// whose credential went bad must not read as DEAD hardware. A config mistake
/// masquerading as a dead recorder is exactly the false alarm the beat rules out.
///
/// Both cost the same thing and it is bounded: anyone already inside
/// WireGuard/the LAN can lie about a queue depth or refresh a beat. Neither
/// grants a read, any audio, or the archive.
const DEVICE_EXEMPT: &[(&str, &str)] = &[
    ("GET", "/api/capture"),
    ("GET", "/api/sources"),
    ("POST", "/api/capture/pause"),
    ("POST", "/api/capture/resume"),
    ("POST", "/api/log"),
    ("POST", "/api/devices/outbox"),
    ("POST", "/api/devices/heartbeat"),
];

/// Where a `RECALL_DEVICE_TOKEN` bearer may stand in for a cookie. A closed set,
/// not "a token is as good as a session anywhere": the credential on the phone
/// grants exactly what the phone does. A phone that can upload a recording still
/// cannot read the household's transcripts.
const DEVICE_TOKEN_PATHS: &[(&str, &str)] = &[("POST", "/api/sessions")];

/// Who is signed in — carried entirely in the cookie, with no server-side store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub user_id: String,
    pub display_name: String,
}

/// Anything carried in a signed token expires. The trait exists so `verify` can
/// enforce it centrally: a caller must not be ABLE to forget the expiry check,
/// because forgetting it means accepting a cookie for ever.
trait Expires {
    fn exp(&self) -> i64;
}

/// The claims inside a session token. `sort_keys` in the Python's JSON dump means
/// the field order here must stay alphabetical to produce identical bytes.
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

/// The claims inside an OAuth `state` token.
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

/// True when this request must carry a valid session: the browsing plane only.
/// Static assets, the OAuth routes and `/sync/*` (its own bearer) are not gated.
#[must_use]
pub fn requires_session(method: &str, path: &str) -> bool {
    if !path.starts_with("/api/") {
        return false;
    }
    let m = method.to_ascii_uppercase();
    !DEVICE_EXEMPT.iter().any(|(em, ep)| *em == m && *ep == path)
}

/// True when a device bearer may stand in for a session on this route.
#[must_use]
pub fn accepts_device_token(method: &str, path: &str) -> bool {
    let m = method.to_ascii_uppercase();
    DEVICE_TOKEN_PATHS
        .iter()
        .any(|(dm, dp)| *dm == m && *dp == path)
}

/// A safe local redirect target — a single-slash absolute path only.
///
/// ⚠ Anything that could leave the origin (`//host`, a scheme) collapses to `/`,
/// so a crafted `?return_to=` cannot turn signing in into an open redirect.
#[must_use]
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

/// `<payload>.<mac>` — the payload base64url-encoded, and an HMAC-SHA256 of that
/// encoding keyed by the secret. Stateless: verifying needs the secret and
/// nothing else.
fn sign<T: Serialize>(secret: &str, claims: &T) -> Option<String> {
    // Compact separators and sorted keys, to match the Python byte for byte.
    let json = serde_json::to_string(claims).ok()?;
    let encoded = b64e(json.as_bytes());
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(encoded.as_bytes());
    Some(format!("{encoded}.{}", b64e(&mac.finalize().into_bytes())))
}

/// The payload if the MAC checks out and the token has not expired, else None.
///
/// ⚠ The MAC is compared in constant time, and every malformed input returns None
/// rather than raising — a forged cookie must not be distinguishable from a
/// corrupt one by timing or by error.
fn verify<T: for<'de> Deserialize<'de> + Expires>(
    secret: &str,
    token: &str,
    now: i64,
) -> Option<T> {
    let (encoded, presented) = token.split_once('.')?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(encoded.as_bytes());
    // `verify_slice` is the constant-time compare; a wrong length is just a miss.
    mac.verify_slice(&b64d(presented)?).ok()?;
    let raw = b64d(encoded)?;
    let claims: T = serde_json::from_slice(&raw).ok()?;
    // Centrally, so no caller can skip it.
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

/// The validated `return_to` if the state token is authentic and fresh, else None
/// — a rejected login, because the state expired or was forged.
#[must_use]
pub fn read_state(secret: &str, token: &str, now: i64) -> Option<String> {
    let claims: StateClaims = verify(secret, token, now)?;
    Some(validate_return_to(Some(&claims.rt)))
}

/// The gate's configuration. Absent means the gate is OFF and recall runs as an
/// open LAN UI — the repo's standing inert-unless-configured pattern, so dev,
/// tests and the Mac's own UI need no ceremony.
#[derive(Debug, Clone)]
pub struct Config {
    pub session_secret: String,
    pub client_id: String,
    pub client_secret: String,
    pub nc_base_url: String,
    pub redirect_uri: String,
    /// Empty = any authenticated Nextcloud user. recall holds household and
    /// medical audio, so the fleet sets this and it is single-user by default.
    pub allowed_users: HashSet<String>,
    pub device_token: Option<String>,
}

impl Config {
    /// The gate goes up only when the whole OAuth triple is present. A partial
    /// configuration is treated as OFF rather than as an error: half a gate that
    /// refuses everyone would take the UI down, and this must never be the reason
    /// a household cannot read its own archive.
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
            redirect_uri: get("NC_REDIRECT_URI")
                .unwrap_or_else(|| "http://10.100.0.2:8000/auth/callback".to_owned()),
            allowed_users: get("RECALL_ALLOWED_USERS")
                .unwrap_or_else(|| "pippijn".to_owned())
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            device_token: get("RECALL_DEVICE_TOKEN").filter(|t| !t.is_empty()),
        })
    }

    /// Whether a signed-in Nextcloud user may enter. Empty allowlist = anyone
    /// who authenticated.
    #[must_use]
    pub fn permits(&self, user_id: &str) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.contains(user_id)
    }

    /// Whether this request carries an acceptable device bearer for this route.
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
        // Constant-time: a token is a secret, and comparing it with `==` leaks
        // its prefix to anyone who can time the answer.
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

/// The Nextcloud authorization URL a sign-in redirects to.
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
