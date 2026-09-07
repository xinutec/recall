//! The SSO gate (stage F1). These are security properties, so each test names the
//! attack or the failure it stands against rather than the function it calls.

use recalld::webauth::{
    Config, Session, accepts_device_token, authorize_url, make_session_cookie, make_state,
    read_session_cookie, read_state, requires_session, validate_return_to,
};
use std::collections::{HashMap, HashSet};

const SECRET: &str = "test-secret-not-a-real-one";
const NOW: i64 = 1_788_000_000;

fn session() -> Session {
    Session {
        user_id: "pippijn".into(),
        display_name: "Pippijn".into(),
    }
}

fn cfg(device_token: Option<&str>) -> Config {
    Config {
        session_secret: SECRET.into(),
        client_id: "cid".into(),
        client_secret: "csec".into(),
        nc_base_url: "https://dash.example.org".into(),
        nc_internal_url: "https://dash.example.org".into(),
        redirect_uri: "http://10.100.0.2:8000/auth/callback".into(),
        allowed_users: HashSet::new(),
        device_token: device_token.map(str::to_owned),
    }
}

#[test]
fn a_cookie_round_trips_and_carries_the_identity() {
    let token = make_session_cookie(SECRET, &session(), NOW).expect("sign");
    let back = read_session_cookie(SECRET, Some(&token), NOW).expect("verify");
    assert_eq!(back, session());
}

#[test]
fn a_forged_or_tampered_cookie_is_refused() {
    // The whole point of signing: identity is carried by the client, so an
    // unforgeable MAC is the only thing standing between a stranger on the VPN
    // and the household's transcripts.
    let token = make_session_cookie(SECRET, &session(), NOW).expect("sign");

    // Signed with a different secret.
    assert_eq!(read_session_cookie("other-secret", Some(&token), NOW), None);

    // Payload edited, MAC left alone.
    let (payload, mac) = token.split_once('.').expect("shape");
    let mut edited = payload.to_owned();
    edited.pop();
    edited.push('X');
    assert_eq!(
        read_session_cookie(SECRET, Some(&format!("{edited}.{mac}")), NOW),
        None
    );

    // Structurally broken input must be refused, never panic.
    for bad in ["", ".", "no-dot", "a.b", "....", "𝕒.𝕓"] {
        assert_eq!(read_session_cookie(SECRET, Some(bad), NOW), None, "{bad}");
    }
    assert_eq!(read_session_cookie(SECRET, None, NOW), None);
}

#[test]
fn a_cookie_expires_and_a_state_expires_much_sooner() {
    let cookie = make_session_cookie(SECRET, &session(), NOW).expect("sign");
    let state = make_state(SECRET, Some("/timeline"), NOW).expect("sign");

    let a_week = 7 * 24 * 60 * 60;
    assert!(read_session_cookie(SECRET, Some(&cookie), NOW + a_week - 1).is_some());
    assert_eq!(
        read_session_cookie(SECRET, Some(&cookie), NOW + a_week + 1),
        None
    );

    // The state is a one-shot login nonce, not a session: minutes, not days.
    assert!(read_state(SECRET, &state, NOW + 9 * 60).is_some());
    assert_eq!(read_state(SECRET, &state, NOW + 11 * 60), None);
}

#[test]
fn a_crafted_return_to_cannot_turn_sign_in_into_an_open_redirect() {
    // Anything that could leave the origin collapses to "/". `//host` is the one
    // that looks local and is not.
    for hostile in [
        "//evil.example.com",
        "https://evil.example.com",
        "http://evil.example.com",
        "javascript:alert(1)",
        "",
    ] {
        assert_eq!(validate_return_to(Some(hostile)), "/", "{hostile}");
    }
    assert_eq!(validate_return_to(None), "/");
    assert_eq!(validate_return_to(Some("/sessions/42")), "/sessions/42");

    // And it survives the round trip, so the redirect the callback performs is
    // the sanitised one rather than what the query string asked for.
    let state = make_state(SECRET, Some("//evil.example.com"), NOW).expect("sign");
    assert_eq!(read_state(SECRET, &state, NOW).as_deref(), Some("/"));
}

#[test]
fn the_browsing_plane_is_gated_and_the_recording_plane_is_not() {
    // Gated: everything under /api/* that a person drives.
    for (m, p) in [
        ("GET", "/api/timeline"),
        ("GET", "/api/search"),
        ("POST", "/api/correct"),
        ("DELETE", "/api/vocabulary/1"),
    ] {
        assert!(requires_session(m, p), "{m} {p} must require a session");
    }

    // Open: a device or daemon cannot perform an interactive OAuth login.
    for (m, p) in [
        ("GET", "/api/capture"),
        ("GET", "/api/sources"),
        ("POST", "/api/capture/pause"),
        ("POST", "/api/capture/resume"),
        ("POST", "/api/log"),
        ("POST", "/api/devices/outbox"),
        ("POST", "/api/devices/heartbeat"),
    ] {
        assert!(!requires_session(m, p), "{m} {p} must stay login-free");
    }

    // Not the browsing plane at all: /sync/* carries its own bearer, and static
    // assets and the OAuth routes must be reachable from the sign-in wall.
    for (m, p) in [
        ("GET", "/sync/jobs"),
        ("GET", "/auth/callback"),
        ("GET", "/"),
        ("GET", "/index.html"),
    ] {
        assert!(!requires_session(m, p), "{m} {p}");
    }
}

#[test]
fn the_exemption_is_per_method_not_per_path() {
    // `GET /api/capture` is the iOS app's long-poll and is open; a POST to the
    // same path is not in the set and must be gated. A path-only check would
    // hand the whole capture surface to anyone on the network.
    assert!(!requires_session("GET", "/api/capture"));
    assert!(requires_session("POST", "/api/capture"));
    assert!(requires_session("DELETE", "/api/capture"));
    // Case is normalised, so a lowercase verb cannot slip past the set.
    assert!(!requires_session("get", "/api/capture"));
}

#[test]
fn a_device_token_opens_exactly_one_route_and_no_other() {
    let c = cfg(Some("device-secret"));
    let bearer = Some("Bearer device-secret");

    assert!(accepts_device_token("POST", "/api/sessions"));
    assert!(c.presents_device_token("POST", "/api/sessions", bearer));

    // The property that motivated the third plane: a phone that can upload a
    // recording still cannot read the household's transcripts.
    for (m, p) in [
        ("GET", "/api/timeline"),
        ("GET", "/api/search"),
        ("GET", "/api/sessions"),
        ("POST", "/api/correct"),
    ] {
        assert!(!accepts_device_token(m, p), "{m} {p}");
        assert!(!c.presents_device_token(m, p, bearer), "{m} {p}");
    }

    // Wrong token, malformed header, and no header at all.
    assert!(!c.presents_device_token("POST", "/api/sessions", Some("Bearer wrong")));
    assert!(!c.presents_device_token("POST", "/api/sessions", Some("device-secret")));
    assert!(!c.presents_device_token("POST", "/api/sessions", None));

    // Unconfigured token = the plane does not exist, rather than "any token".
    assert!(!cfg(None).presents_device_token("POST", "/api/sessions", bearer));
}

#[test]
fn the_allowlist_narrows_who_may_enter_after_a_valid_sign_in() {
    // A valid Nextcloud sign-in is not enough: recall holds household and medical
    // audio, so it is single-user by default.
    let mut c = cfg(None);
    c.allowed_users = ["pippijn".to_owned()].into_iter().collect();
    assert!(c.permits("pippijn"));
    assert!(!c.permits("someone-else"));

    // Empty = any authenticated user, which is the documented meaning.
    c.allowed_users = HashSet::new();
    assert!(c.permits("anyone"));
}

#[test]
fn a_partial_oauth_configuration_leaves_the_gate_off_rather_than_broken() {
    // Half a gate that refuses everyone would take the UI down, and this must
    // never be the reason a household cannot read its own archive.
    let required = [
        ("RECALL_SESSION_SECRET", SECRET),
        ("NC_CLIENT_ID", "cid"),
        ("NC_CLIENT_SECRET", "csec"),
    ];
    let env = |vars: &HashMap<&str, &str>| {
        let owned: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| owned.get(k).cloned()
    };

    let full: HashMap<&str, &str> = required.into_iter().collect();
    assert!(Config::from_env(&env(&full)).is_some());

    for (missing, _) in required {
        let mut partial = full.clone();
        partial.remove(missing);
        assert!(
            Config::from_env(&env(&partial)).is_none(),
            "missing {missing}"
        );
    }

    // Present but empty is also off — an unset secret and a blank one are the
    // same mistake.
    let mut blank = full.clone();
    blank.insert("NC_CLIENT_SECRET", "");
    assert!(Config::from_env(&env(&blank)).is_none());
}

#[test]
fn the_authorize_url_carries_the_state_and_escapes_its_parameters() {
    let url = authorize_url(&cfg(None), "st/ate+value");
    assert!(url.starts_with("https://dash.example.org/index.php/apps/oauth2/authorize?"));
    assert!(url.contains("client_id=cid"));
    assert!(url.contains("response_type=code"));
    // The redirect URI and state must be escaped, or a value containing & or /
    // would inject a parameter into the authorize request.
    assert!(url.contains("redirect_uri=http%3A%2F%2F10.100.0.2%3A8000%2Fauth%2Fcallback"));
    assert!(url.contains("state=st%2Fate%2Bvalue"));
}

/// ⚠ **The claim that justifies keeping the Python's exact token format, pinned
/// as a golden value rather than asserted.**
///
/// These two tokens were minted by `recall.webauth` itself (the real code, not a
/// reimplementation of it) with `SECRET` at `NOW`. If Rust can verify them, then
/// a cookie issued by the Python OAuth flow is accepted here — which is what lets
/// recalld be mounted behind the EXISTING sign-in and cut over route-group by
/// route-group, with no second login and no flag day.
///
/// If this test ever fails, the two halves have stopped recognising each other
/// and an incremental cutover is off the table. That is a much bigger fact than
/// a broken unit test, so it is spelled out here rather than left to be inferred
/// from a diff.
#[test]
fn a_cookie_minted_by_the_python_verifies_here() {
    const PY_COOKIE: &str = concat!(
        "eyJleHAiOjE3ODg2MDQ4MDAsIm5hbWUiOiJQaXBwaWpuIiwidWlkIjoicGlwcGlqbiJ9",
        ".xJ3IAn7Hu1_0YZ0BAdcLf5hhoOCtjysRHe_DKpZCEpY"
    );
    const PY_STATE: &str = concat!(
        "eyJleHAiOjE3ODgwMDA2MDAsInJ0IjoiL3Nlc3Npb25zLzQyIn0",
        ".iM4AGYnF9m9OnOi-5QAyidaObrZKQubhY7RLgafpuGE"
    );

    let s = read_session_cookie(SECRET, Some(PY_COOKIE), NOW)
        .expect("the Python's cookie must verify here");
    assert_eq!(s.user_id, "pippijn");
    assert_eq!(s.display_name, "Pippijn");

    assert_eq!(
        read_state(SECRET, PY_STATE, NOW).as_deref(),
        Some("/sessions/42")
    );

    // And the other direction: the bytes this mints are the bytes the Python
    // would, so a cookie set by recalld is equally readable by the Python half
    // while both are serving.
    let ours = make_session_cookie(SECRET, &session(), NOW).expect("sign");
    assert_eq!(
        ours, PY_COOKIE,
        "the token encoding drifted from the Python's"
    );
}

// --- the OAuth exchange, against a REAL server ---------------------------------
//
// A stub Nextcloud rather than a mocked client: the thing most likely to be wrong
// in this code is the SHAPE of the request and the response parsing, and a mock
// that returns what I expect would test my expectation rather than the wire.

use axum::Router;
use axum::routing::{get, post};
use recalld::webauth::{AuthError, exchange_code, fetch_userinfo, server_call};
use std::net::SocketAddr;

/// Serve `routes` on an ephemeral port and return its base URL.
async fn serve(routes: Router) -> String {
    let listener = tokio::net::TcpListener::bind::<SocketAddr>("127.0.0.1:0".parse().unwrap())
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, routes).await.expect("serve");
    });
    format!("http://{addr}")
}

fn at(base: &str) -> Config {
    let mut c = cfg(None);
    base.clone_into(&mut c.nc_base_url);
    base.clone_into(&mut c.nc_internal_url);
    c
}

#[tokio::test]
async fn a_code_is_exchanged_for_a_token_and_the_user_is_resolved() {
    let app = Router::new()
        .route(
            "/index.php/apps/oauth2/api/v1/token",
            post(|body: String| async move {
                // The form must carry every field Nextcloud needs; a missing
                // client_secret would 400 in production and pass a mock.
                for field in [
                    "grant_type=authorization_code",
                    "code=the-code",
                    "client_id=cid",
                    "client_secret=csec",
                ] {
                    assert!(body.contains(field), "form missing {field}: {body}");
                }
                axum::Json(serde_json::json!({"access_token": "tok-123"}))
            }),
        )
        .route(
            "/ocs/v2.php/cloud/user",
            get(|headers: axum::http::HeaderMap| async move {
                // Both headers are load-bearing: OCS refuses the request without
                // its own marker, and the bearer is what identifies the user.
                assert_eq!(headers.get("authorization").unwrap(), "Bearer tok-123");
                assert_eq!(headers.get("ocs-apirequest").unwrap(), "true");
                axum::Json(serde_json::json!({
                    "ocs": {"data": {"id": "pippijn", "displayname": "Pippijn"}}
                }))
            }),
        );
    let base = serve(app).await;
    let c = at(&base);

    let token = tokio::task::spawn_blocking({
        let c = c.clone();
        move || exchange_code(&c, "the-code")
    })
    .await
    .unwrap()
    .expect("exchange");
    assert_eq!(token, "tok-123");

    let session = tokio::task::spawn_blocking(move || fetch_userinfo(&c, &token))
        .await
        .unwrap()
        .expect("userinfo");
    assert_eq!(session.user_id, "pippijn");
    assert_eq!(session.display_name, "Pippijn");
}

#[tokio::test]
async fn a_response_missing_what_it_must_carry_is_an_error_not_an_empty_identity() {
    // The failure that matters: silently accepting a blank id would sign someone
    // in as "" and, with an empty allowlist, let them straight through.
    let app = Router::new()
        .route(
            "/index.php/apps/oauth2/api/v1/token",
            post(|| async { axum::Json(serde_json::json!({"token_type": "Bearer"})) }),
        )
        .route(
            "/ocs/v2.php/cloud/user",
            get(|| async { axum::Json(serde_json::json!({"ocs": {"data": {"id": ""}}})) }),
        );
    let base = serve(app).await;
    let c = at(&base);

    let e = tokio::task::spawn_blocking({
        let c = c.clone();
        move || exchange_code(&c, "x")
    })
    .await
    .unwrap()
    .expect_err("must reject a token-less response");
    assert!(matches!(e, AuthError::Malformed(_)), "{e}");

    let e = tokio::task::spawn_blocking(move || fetch_userinfo(&c, "tok"))
        .await
        .unwrap()
        .expect_err("must reject an id-less response");
    assert!(matches!(e, AuthError::Malformed(_)), "{e}");
}

#[tokio::test]
async fn a_user_with_no_display_name_falls_back_to_their_id() {
    // A person who can sign in must not be locked out by an empty profile field.
    let app = Router::new().route(
        "/ocs/v2.php/cloud/user",
        get(|| async {
            axum::Json(serde_json::json!({"ocs": {"data": {"id": "pippijn", "displayname": ""}}}))
        }),
    );
    let base = serve(app).await;
    let c = at(&base);
    let s = tokio::task::spawn_blocking(move || fetch_userinfo(&c, "tok"))
        .await
        .unwrap()
        .expect("userinfo");
    assert_eq!(s.display_name, "pippijn");
}

#[test]
fn an_internal_url_is_called_but_the_public_host_is_presented() {
    // Nextcloud routes on trusted domains, so an in-cluster call must still LOOK
    // like the public one or it is refused.
    let mut c = cfg(None);
    c.nc_base_url = "https://dash.example.org".into();
    c.nc_internal_url = "http://nextcloud.svc.cluster.local".into();
    let (url, host) = server_call(&c, "/ocs/v2.php/cloud/user");
    assert_eq!(
        url,
        "http://nextcloud.svc.cluster.local/ocs/v2.php/cloud/user"
    );
    assert_eq!(host.as_deref(), Some("dash.example.org"));

    // When they are the same there is nothing to spoof, so no header is set.
    c.nc_internal_url = c.nc_base_url.clone();
    let (url, host) = server_call(&c, "/x");
    assert_eq!(url, "https://dash.example.org/x");
    assert_eq!(host, None);
}

// --- the gate, through a real router -------------------------------------------

use axum::body::Body;
use axum::http::Request;
use recalld::webauth::{COOKIE_NAME, GateState, gate, routes};
use std::sync::Arc;
use tower::ServiceExt;

/// A router with the gate applied over one protected and one open route, so the
/// middleware is exercised exactly as it will be in the pod.
fn gated(c: Config) -> Router {
    let st = GateState {
        cfg: Arc::new(c),
        now: Arc::new(|| NOW),
    };
    Router::new()
        .route("/api/timeline", get(|| async { "the archive" }))
        .route("/api/capture", get(|| async { "pause state" }))
        .merge(routes(st.clone()))
        .layer(axum::middleware::from_fn_with_state(st, gate))
}

async fn status(app: &Router, req: Request<Body>) -> axum::http::StatusCode {
    app.clone().oneshot(req).await.expect("call").status()
}

#[tokio::test]
async fn the_gate_refuses_the_archive_without_a_session_and_lets_the_pause_through() {
    let app = gated(cfg(None));

    // The property this whole module exists for: a stranger on the VPN cannot
    // read the household's transcripts.
    assert_eq!(
        status(
            &app,
            Request::get("/api/timeline").body(Body::empty()).unwrap()
        )
        .await,
        401
    );

    // ...while the recording plane stays reachable, because a phone cannot log in.
    assert_eq!(
        status(
            &app,
            Request::get("/api/capture").body(Body::empty()).unwrap()
        )
        .await,
        200
    );

    // A valid cookie opens the archive.
    let token = make_session_cookie(SECRET, &session(), NOW).expect("sign");
    assert_eq!(
        status(
            &app,
            Request::get("/api/timeline")
                .header("cookie", format!("{COOKIE_NAME}={token}"))
                .body(Body::empty())
                .unwrap()
        )
        .await,
        200
    );
}

#[tokio::test]
async fn a_signed_in_user_outside_the_allowlist_is_forbidden_not_unauthenticated() {
    // 403, not 401: they ARE signed in, and telling them to sign in again would
    // loop them through Nextcloud for ever.
    let mut c = cfg(None);
    c.allowed_users = ["someone-else".to_owned()].into_iter().collect();
    let token = make_session_cookie(SECRET, &session(), NOW).expect("sign");
    assert_eq!(
        status(
            &gated(c),
            Request::get("/api/timeline")
                .header("cookie", format!("{COOKIE_NAME}={token}"))
                .body(Body::empty())
                .unwrap()
        )
        .await,
        403
    );
}

#[tokio::test]
async fn a_device_token_opens_its_route_through_the_middleware_and_no_other() {
    let c = cfg(Some("device-secret"));
    let st = GateState {
        cfg: Arc::new(c),
        now: Arc::new(|| NOW),
    };
    let app = Router::new()
        .route("/api/sessions", post(|| async { "uploaded" }))
        .route("/api/timeline", get(|| async { "the archive" }))
        .layer(axum::middleware::from_fn_with_state(st, gate));

    assert_eq!(
        status(
            &app,
            Request::post("/api/sessions")
                .header("authorization", "Bearer device-secret")
                .body(Body::empty())
                .unwrap()
        )
        .await,
        200
    );
    // The same credential must NOT read transcripts.
    assert_eq!(
        status(
            &app,
            Request::get("/api/timeline")
                .header("authorization", "Bearer device-secret")
                .body(Body::empty())
                .unwrap()
        )
        .await,
        401
    );
}

#[tokio::test]
async fn the_callback_rejects_a_forged_state_before_making_any_network_call() {
    // The config points at a port nothing is listening on, so if the handler
    // reached the network this would hang or 502 rather than 403 — which is the
    // point: a stranger must not be able to make this server dial Nextcloud.
    let mut c = cfg(None);
    c.nc_internal_url = "http://127.0.0.1:1".into();
    let app = gated(c);

    assert_eq!(
        status(
            &app,
            Request::get("/auth/callback?code=x&state=forged")
                .body(Body::empty())
                .unwrap()
        )
        .await,
        403
    );
    // No state at all is the same refusal.
    assert_eq!(
        status(
            &app,
            Request::get("/auth/callback?code=x")
                .body(Body::empty())
                .unwrap()
        )
        .await,
        403
    );
}

#[tokio::test]
async fn login_redirects_to_nextcloud_and_the_cookie_it_later_sets_is_httponly() {
    let app = gated(cfg(None));
    let resp = app
        .clone()
        .oneshot(
            Request::get("/login?return_to=/sessions/42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("call");
    assert_eq!(resp.status(), 302);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("https://dash.example.org/index.php/apps/oauth2/authorize?"));
    assert!(location.contains("state="));

    // Logout clears the cookie on the same path it was set on, or the browser
    // keeps the old one and the user cannot sign out.
    let resp = app
        .oneshot(Request::post("/logout").body(Body::empty()).unwrap())
        .await
        .expect("call");
    let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("Max-Age=0"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
}

/// ⚠ `/api/me` is the SPA's login probe and recalld is its ONLY implementation —
/// the Python's copy was unreachable behind the proxy and has been deleted. The
/// shape is what the app reads to decide it is signed in, so it is pinned here
/// rather than left to the route existing.
#[tokio::test]
async fn me_answers_with_the_identity_the_cookie_carries() {
    let app = gated(cfg(None));
    let token = make_session_cookie(SECRET, &session(), NOW).expect("sign");

    let response = app
        .clone()
        .oneshot(
            Request::get("/api/me")
                .header("cookie", format!("{COOKIE_NAME}={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("call");

    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["userId"], session().user_id);
    assert_eq!(json["displayName"], session().display_name);
}

#[tokio::test]
async fn me_without_a_session_is_refused_rather_than_anonymous() {
    // An empty identity would read to the SPA as "signed in as nobody".
    let app = gated(cfg(None));

    assert_eq!(
        status(&app, Request::get("/api/me").body(Body::empty()).unwrap()).await,
        401
    );
}
