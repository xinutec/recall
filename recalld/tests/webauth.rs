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
