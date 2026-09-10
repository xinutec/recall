//! Uploading a meeting — use case 2's front door.

use chrono::{DateTime, Utc};
use recalld::upload::{
    Media, is_supported, london_offset_hours, meeting_id, register, started_at, stored_path,
    suffix_of,
};
use rusqlite::Connection;

fn at(iso: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(iso)
        .expect("iso")
        .with_timezone(&Utc)
}

#[test]
fn a_meeting_id_is_its_london_start_not_the_containers_utc() {
    // ⚠ THE TRAP. The pod runs UTC. A July recording at 09:50 LONDON is 08:50
    // UTC, so deriving the id from the container clock names it meeting-…-0850
    // and it no longer matches the directory the worker discovered.
    let summer = at("2026-07-03T08:50:00+00:00");
    assert_eq!(
        london_offset_hours(summer),
        1,
        "BST, so the zone is doing work"
    );

    let (id, title) = meeting_id(summer);

    assert_eq!(id, "meeting-20260703-0950");
    assert_eq!(title, "Meeting 2026-07-03 09:50");
}

#[test]
fn a_winter_meeting_keeps_utc_because_london_is_utc_then() {
    let winter = at("2026-01-15T09:50:00+00:00");
    assert_eq!(london_offset_hours(winter), 0);

    let (id, _) = meeting_id(winter);

    assert_eq!(id, "meeting-20260115-0950");
}

#[test]
fn the_stored_filename_uses_the_utc_stamp_not_the_local_one() {
    // The id is local, the FILENAME's stamp is the UTC instant — mirroring the
    // Python, which formats the id from `local` and the path from `started`.
    let started = at("2026-07-03T08:50:00+00:00");

    let path = stored_path(
        std::path::Path::new("/data"),
        "meeting-20260703-0950",
        started,
        ".mp3",
    );

    assert_eq!(
        path,
        std::path::Path::new(
            "/data/meeting-20260703-0950/meeting-20260703-0950-20260703T085000.mp3"
        )
    );
}

#[test]
fn only_containers_worth_decoding_are_accepted() {
    for good in [".mp3", ".m4a", ".flac", ".opus", ".webm"] {
        assert!(is_supported(good), "{good} should be accepted");
    }
    for bad in [".txt", ".pdf", "", ".exe"] {
        assert!(!is_supported(bad), "{bad} should be refused");
    }
}

#[test]
fn a_suffix_is_taken_case_insensitively_and_may_be_absent() {
    assert_eq!(suffix_of("hospital.MP3"), ".mp3");
    assert_eq!(suffix_of("hospital.mp3"), ".mp3");
    assert_eq!(suffix_of("noextension"), "");
    assert_eq!(suffix_of("a.b.m4a"), ".m4a");
}

#[test]
fn an_absent_start_means_now_and_a_malformed_one_is_refused() {
    let now = started_at("").expect("empty means now");
    assert!((Utc::now() - now).num_seconds().abs() < 5);

    assert!(started_at("last tuesday").is_err());
    assert_eq!(
        started_at("2026-07-03T09:50:00+01:00").expect("offset"),
        at("2026-07-03T08:50:00+00:00"),
        "an offset is honoured, not ignored"
    );
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
                               port INTEGER);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL, path TEXT,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, sample_rate INTEGER,
            channels INTEGER,
            -- ⚠ Copied from store_schema, and load-bearing: without it the
            -- INSERT OR IGNORE has nothing to ignore against and a re-upload
            -- silently duplicates the segment. A test schema that omitted it
            -- would pass while testing a table this product does not have.
            UNIQUE (source_id, start_utc));",
    )
    .expect("schema");
    conn
}

const MEDIA: Media = Media {
    duration_s: 4.5,
    sample_rate: 48000,
    channels: 1,
};

#[test]
fn an_upload_corrects_a_kind_the_worker_already_guessed() {
    // ⚠ REGISTER, not add. The worker scans the data root continuously and may
    // have claimed this directory with a DISCOVERED kind. An INSERT OR IGNORE
    // would leave that standing — and the sessions list selects on kind='upload',
    // so the meeting would never appear and could never be renamed or deleted.
    let conn = db();
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('meeting-20260703-0950',
             'meeting-20260703-0950', 'discovered')",
        [],
    )
    .expect("the worker got there first");

    register(
        &conn,
        "meeting-20260703-0950",
        "Dr Lee RT",
        std::path::Path::new("/data/x.mp3"),
        at("2026-07-03T08:50:00+00:00"),
        MEDIA,
    )
    .expect("registered");

    let (kind, name): (String, String) = conn
        .query_row(
            "SELECT kind, name FROM sources WHERE id='meeting-20260703-0950'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read back");
    assert_eq!(kind, "upload");
    assert_eq!(name, "Dr Lee RT", "the placeholder name is replaced");
}

#[test]
fn a_title_the_user_chose_survives_a_re_registration() {
    // ⚠ The other half of the same rule: a name the user typed is theirs, and
    // re-registering must not rename it back.
    let conn = db();
    register(
        &conn,
        "m",
        "Neuro-oncology clinic",
        std::path::Path::new("/data/x.mp3"),
        at("2026-07-03T08:50:00+00:00"),
        MEDIA,
    )
    .expect("first");

    register(
        &conn,
        "m",
        "Meeting 2026-07-03 09:50",
        std::path::Path::new("/data/x.mp3"),
        at("2026-07-03T08:50:00+00:00"),
        MEDIA,
    )
    .expect("again");

    let name: String = conn
        .query_row("SELECT name FROM sources WHERE id='m'", [], |r| r.get(0))
        .expect("read back");
    assert_eq!(name, "Neuro-oncology clinic");
}

#[test]
fn the_segments_span_is_the_measured_duration() {
    let conn = db();

    register(
        &conn,
        "m",
        "M",
        std::path::Path::new("/data/x.mp3"),
        at("2026-07-03T08:50:00+00:00"),
        MEDIA,
    )
    .expect("registered");

    let (start, end, rate, ch): (String, String, i64, i64) = conn
        .query_row(
            "SELECT start_utc, end_utc, sample_rate, channels FROM audio_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("segment");
    assert_eq!(start, "2026-07-03T08:50:00+00:00");
    assert_eq!(
        end, "2026-07-03T08:50:04.500000+00:00",
        "spelled as isoformat writes it"
    );
    assert_eq!(rate, 48000);
    assert_eq!(ch, 1);
}

#[test]
fn re_registering_the_same_segment_does_not_duplicate_it() {
    let conn = db();
    for _ in 0..3 {
        register(
            &conn,
            "m",
            "M",
            std::path::Path::new("/data/x.mp3"),
            at("2026-07-03T08:50:00+00:00"),
            MEDIA,
        )
        .expect("registered");
    }

    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM audio_segments", [], |r| r.get(0))
        .expect("count");
    assert_eq!(n, 1);
}

// --- through the router, with real audio --------------------------------------

use axum::body::Body;
use axum::http::Request;
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::webauth::{self, COOKIE_NAME, GateState, Session, make_session_cookie};
use std::sync::Arc;
use tower::ServiceExt;

fn gated(root: &std::path::Path) -> axum::Router {
    router(Arc::new(Config {
        root: root.to_path_buf(),
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        webauth: Some(GateState {
            cfg: Arc::new(webauth::Config {
                session_secret: SECRET.into(),
                client_id: "cid".into(),
                client_secret: "csec".into(),
                nc_base_url: "https://dash.example.org".into(),
                nc_internal_url: "https://dash.example.org".into(),
                redirect_uri: "http://127.0.0.1/auth/callback".into(),
                allowed_users: std::collections::HashSet::new(),
                device_token: None,
            }),
            now: Arc::new(|| NOW),
        }),
        sync_token: None,
        upstream: None,
        frontend: None,
    }))
}

/// A real, decodable file — ffmpeg makes it, so the probe path is exercised
/// rather than mocked.
fn make_flac(path: &std::path::Path, seconds: f64) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-ar", "48000", "-ac", "1"])
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

fn multipart(filename: &str, bytes: &[u8], title: &str, start: &str) -> (String, Vec<u8>) {
    let boundary = "----recalldtestboundary";
    let mut body = Vec::new();
    let mut part = |name: &str, value: &str| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    };
    if !title.is_empty() {
        part("title", title);
    }
    if !start.is_empty() {
        part("start", start);
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"audio\"; \
             filename=\"{filename}\"\r\nContent-Type: audio/flac\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

const SECRET: &str = "test-secret-not-a-real-one";
const NOW: i64 = 1_788_000_000;

/// The cookie a signed-in browser holds. Uploading is a browsing-plane write, so
/// it is gated — unlike a phone's heartbeat, a person does this from the UI.
fn cookie() -> String {
    let session = Session {
        user_id: "pippijn".into(),
        display_name: "Pippijn".into(),
    };
    format!(
        "{COOKIE_NAME}={}",
        make_session_cookie(SECRET, &session, NOW).expect("sign")
    )
}

fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
                               port INTEGER);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL, path TEXT,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, sample_rate INTEGER,
            channels INTEGER, UNIQUE (source_id, start_utc));",
    )
    .expect("schema");
    recalld::store::open(dir.path()).expect("ingest db");
    dir
}

#[tokio::test]
async fn a_real_recording_uploads_and_becomes_a_session() {
    let dir = scratch();
    let clip = dir.path().join("hospital.flac");
    if !make_flac(&clip, 2.0) {
        eprintln!("skipped: no ffmpeg on this host");
        return;
    }
    let bytes = std::fs::read(&clip).expect("read");
    let (ctype, body) = multipart(
        "hospital.flac",
        &bytes,
        "Dr Lee RT",
        "2026-07-03T09:50:00+01:00",
    );

    let response = gated(dir.path())
        .oneshot(
            Request::post("/api/sessions")
                .header("content-type", ctype)
                .header("cookie", cookie())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("call");

    assert_eq!(response.status(), 200);
    let out = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json");
    assert_eq!(json["id"], "meeting-20260703-0950", "the LONDON id");
    assert_eq!(json["title"], "Dr Lee RT");
    assert_eq!(
        json["turnCount"], 0,
        "it appears at once, before transcription"
    );

    // The file landed in its own directory, and the source is an upload.
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let (kind, path): (String, String) = conn
        .query_row(
            "SELECT s.kind, a.path FROM sources s JOIN audio_segments a ON a.source_id = s.id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row");
    assert_eq!(kind, "upload");
    assert!(
        std::path::Path::new(&path).exists(),
        "the audio is on disk at {path}"
    );
}

#[tokio::test]
async fn a_file_that_is_not_audio_is_refused_and_leaves_nothing_behind() {
    // ⚠ The suffix gate passes here — it is named .flac — so this exercises the
    // PROBE's refusal, and the cleanup that must follow it. A file left on disk
    // would be found by the worker and registered as a source of its own.
    let dir = scratch();
    let (ctype, body) = multipart("hospital.flac", b"this is not audio at all", "", "");

    let response = gated(dir.path())
        .oneshot(
            Request::post("/api/sessions")
                .header("content-type", ctype)
                .header("cookie", cookie())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("call");

    assert_eq!(response.status(), 400);
    let conn = Connection::open(dir.path().join("recall.sqlite")).expect("db");
    let sources: i64 = conn
        .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))
        .expect("count");
    assert_eq!(sources, 0, "no half-made session");
    let stray: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir() && e.file_name().to_string_lossy().starts_with("meeting-"))
        .flat_map(|e| {
            std::fs::read_dir(e.path())
                .expect("readdir")
                .filter_map(Result::ok)
        })
        .collect();
    assert!(
        stray.is_empty(),
        "the unreadable file must not be left for the worker"
    );
}

#[tokio::test]
async fn an_unsupported_container_is_refused_before_it_is_written() {
    let dir = scratch();
    let (ctype, body) = multipart("notes.txt", b"hello", "", "");

    let response = gated(dir.path())
        .oneshot(
            Request::post("/api/sessions")
                .header("content-type", ctype)
                .header("cookie", cookie())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("call");

    assert_eq!(response.status(), 400);
}

// --- the autumn clock change ------------------------------------------------

/// ⚠ **Two recordings became one, and nothing errored** (#1476). A meeting's id
/// is its LOCAL start, and on the night the clocks go back the local hour
/// 01:00–02:00 happens TWICE, so two different UTC instants derive one id:
///
///     2026-10-25T00:30:00Z  ->  local 01:30 BST
///     2026-10-25T01:30:00Z  ->  local 01:30 GMT
///
/// The second upload then registered the id the first already held (an UPSERT,
/// not a refusal), its segment landed under the same source because the start
/// times differ, and its audio was written into the first meeting's directory.
/// One session, two recordings, no error anywhere.
#[test]
fn the_two_local_half_past_ones_on_the_autumn_change_are_different_meetings() {
    let first = "2026-10-25T00:30:00Z".parse().expect("first");
    let second = "2026-10-25T01:30:00Z".parse().expect("second");

    let (id_a, title_a) = meeting_id(first);
    let (id_b, title_b) = meeting_id(second);

    assert_ne!(id_a, id_b, "an hour apart must not be one meeting");
    assert_ne!(title_a, title_b, "two sessions must not read identically");
}

/// ⚠ The fix must not RENAME anything that already exists. The first occurrence
/// is the one every earlier meeting is spelled like, so it keeps the plain id;
/// only the repeat of the hour is marked.
#[test]
fn the_first_pass_through_the_repeated_hour_keeps_the_plain_id() {
    let first = "2026-10-25T00:30:00Z".parse().expect("first");

    let (id, title) = meeting_id(first);

    assert_eq!(id, "meeting-20261025-0130");
    assert_eq!(title, "Meeting 2026-10-25 01:30");
}

/// The second carries the zone it actually happened in, which is the one piece
/// of information that tells the two apart to a person reading a list.
#[test]
fn the_second_pass_is_marked_with_the_zone_it_happened_in() {
    let second = "2026-10-25T01:30:00Z".parse().expect("second");

    let (id, title) = meeting_id(second);

    assert!(id.starts_with("meeting-20261025-0130"), "{id}");
    assert!(id.ends_with("-gmt"), "the repeat is marked: {id}");
    assert!(title.contains("GMT"), "{title}");
}

/// ⚠ Idempotence is what makes this safe to deploy: the SAME recording uploaded
/// twice must still land on one id, or a re-upload mints a second session.
#[test]
fn the_same_instant_always_derives_the_same_id() {
    let at = "2026-10-25T01:30:00Z".parse().expect("at");

    assert_eq!(meeting_id(at), meeting_id(at));
}

/// The spring edge needs no marking and must not get any: the skipped local hour
/// is the local time of no instant at all, so no id is derivable twice.
#[test]
fn the_spring_change_needs_no_marker_because_no_id_repeats() {
    for utc in [
        "2026-03-29T00:30:00Z",
        "2026-03-29T01:30:00Z",
        "2026-03-29T02:30:00Z",
    ] {
        let (id, _) = meeting_id(utc.parse().expect("utc"));
        assert!(!id.ends_with("-gmt"), "{utc} was marked: {id}");
    }
}

/// ⚠ The control. An ordinary winter meeting is ALSO in GMT, and marking those
/// would rename every meeting between November and March.
#[test]
fn an_ordinary_gmt_meeting_is_not_marked() {
    let (id, title) = meeting_id("2026-12-01T01:30:00Z".parse().expect("winter"));

    assert_eq!(
        id, "meeting-20261201-0130",
        "only the AMBIGUOUS hour is marked"
    );
    assert_eq!(title, "Meeting 2026-12-01 01:30");
}

/// ⚠ **The harm end to end, not just the id.** The id being distinct is only
/// half of #1476: what the bug actually did was land BOTH recordings under one
/// source and write the second's audio into the first's directory. This walks
/// the real path — derive, place, register — for two uploads an hour apart in
/// the repeated hour, and asserts they stay two meetings with two files.
#[test]
fn two_uploads_in_the_repeated_hour_stay_two_sessions_with_two_files() {
    let conn = db();
    let root = std::path::Path::new("/data");
    let first = at("2026-10-25T00:30:00+00:00");
    let second = at("2026-10-25T01:30:00+00:00");

    let mut paths = Vec::new();
    for started in [first, second] {
        let (id, title) = meeting_id(started);
        let path = stored_path(root, &id, started, ".mp3");
        register(&conn, &id, &title, &path, started, MEDIA).expect("registered");
        paths.push(path);
    }

    let sources: i64 = conn
        .query_row(
            "SELECT count(*) FROM sources WHERE kind='upload'",
            [],
            |r| r.get(0),
        )
        .expect("count sources");
    assert_eq!(sources, 2, "an hour apart is two meetings, not one");

    // The directory is the source id, so a shared id meant a shared directory.
    let dirs: std::collections::BTreeSet<_> =
        paths.iter().map(|p| p.parent().expect("dir")).collect();
    assert_eq!(
        dirs.len(),
        2,
        "each meeting owns its own directory: {paths:?}"
    );

    // And neither session ends up holding the other's audio.
    for (id, want) in [
        ("meeting-20261025-0130", 1_i64),
        ("meeting-20261025-0130-gmt", 1_i64),
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM audio_segments WHERE source_id = ?1",
                [id],
                |r| r.get(0),
            )
            .expect("count segments");
        assert_eq!(n, want, "{id} should hold exactly its own recording");
    }
}
