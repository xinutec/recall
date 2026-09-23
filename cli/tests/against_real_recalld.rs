//! `recall-cli` against the real recalld router, with the SSO gate on in every
//! test: `recalld::webauth` refuses the browsing routes without a session, and a
//! suite booted with `webauth: None` would pass while the shipped CLI could not
//! read a turn.
//!
//! Everything is real except the archive's contents, which are inserted
//! straight into a temporary database.

use cli::api::Api;
use cli::render;
use recalld::app::{Config as ServerConfig, router};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

const SECRET: &str = "test-session-secret";

/// A recalld serving `root`, with the browsing gate up. Returns its base URL.
fn serve(root: &Path) -> String {
    let webauth = recalld::webauth::GateState {
        cfg: Arc::new(recalld::webauth::Config {
            session_secret: SECRET.to_owned(),
            client_id: "id".to_owned(),
            client_secret: "secret".to_owned(),
            nc_base_url: "http://nextcloud.invalid".to_owned(),
            nc_internal_url: "http://nextcloud.invalid".to_owned(),
            redirect_uri: "http://recall.invalid/auth/callback".to_owned(),
            // Empty means any authenticated user.
            allowed_users: HashSet::new(),
            device_token: None,
        }),
        now: Arc::new(|| 1_700_000_000),
    };
    let config = Arc::new(ServerConfig {
        root: root.to_owned(),
        tokens: None,
        read_token: None,
        max_body_bytes: 16 * 1024 * 1024,
        webauth: Some(webauth),
        sync_token: None,
        frontend: None,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr")).expect("send");
            axum::serve(listener, router(config)).await.expect("serve");
        });
    });
    format!("http://{}", rx.recv().expect("addr"))
}

/// A cookie the gate will accept, minted the way the OAuth callback mints one.
fn session() -> String {
    recalld::webauth::make_session_cookie(
        SECRET,
        &recalld::webauth::Session {
            user_id: "pippijn".to_owned(),
            display_name: "Pippijn".to_owned(),
        },
        1_700_000_000,
    )
    .expect("cookie")
}

/// The minimum `recall.sqlite` the read routes need, plus one turn.
///
/// The schema is hand-written rather than built by
/// `recalld::meaning_schema::ensure`, so it can drift from production's.
fn archive(root: &Path, text: &str, speaker: Option<&str>, guess: Option<(&str, f64)>) -> i64 {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("db");
    conn.execute_batch(
        "CREATE TABLE audio_segments (
             id          INTEGER PRIMARY KEY,
             source_id   TEXT NOT NULL,
             path        TEXT NOT NULL,
             start_utc   TEXT NOT NULL,
             end_utc     TEXT NOT NULL,
             sample_rate INTEGER NOT NULL,
             channels    INTEGER NOT NULL,
             transcribed_utc TEXT, mean_volume REAL, envelope BLOB,
             speech_s REAL, structure REAL, pushed_utc TEXT,
             UNIQUE (source_id, start_utc)
         );
         CREATE TABLE speakers (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE transcript_segments (
             id                  INTEGER PRIMARY KEY,
             audio_segment_id    INTEGER REFERENCES audio_segments(id),
             start_utc           TEXT NOT NULL,
             end_utc             TEXT NOT NULL,
             text                TEXT NOT NULL,
             language            TEXT,
             language_confidence REAL,
             asr_confidence      REAL,
             asr_model           TEXT NOT NULL,
             speaker_label       TEXT,
             speaker_id          INTEGER REFERENCES speakers(id),
             superseded_by       INTEGER REFERENCES transcript_segments(id),
             created_utc         TEXT,
             provenance          TEXT,
             hidden_reason       TEXT,
             loudness            REAL,
             speaker_guess       TEXT,
             speaker_score       REAL,
             speaker_cluster     TEXT,
             word_timings        TEXT
         );
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text);
         CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE capture_events (
             id INTEGER PRIMARY KEY, utc TEXT NOT NULL, kind TEXT NOT NULL,
             source_id TEXT, detail TEXT
         );
         CREATE TABLE corrections (
             id                    INTEGER PRIMARY KEY,
             transcript_segment_id INTEGER REFERENCES transcript_segments(id),
             audio_segment_id      INTEGER REFERENCES audio_segments(id),
             start_utc             TEXT NOT NULL,
             end_utc               TEXT NOT NULL,
             original_text         TEXT NOT NULL,
             corrected_text        TEXT NOT NULL,
             language              TEXT,
             created_utc           TEXT NOT NULL,
             speaker               TEXT,
             hidden_reason         TEXT,
             audio_confidence      REAL
         );
         CREATE TABLE speaker_embeddings (
             id          INTEGER PRIMARY KEY,
             speaker_id  INTEGER NOT NULL REFERENCES speakers(id),
             vector      TEXT NOT NULL,
             created_utc TEXT NOT NULL,
             source_correction_id INTEGER,
             source_segment_id    INTEGER
         );
         CREATE TABLE sources (
             id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
             port INTEGER, event_db REAL, noise_shape BLOB
         );
         INSERT INTO sources (id, name, kind) VALUES ('usb', 'USB mic', 'coreaudio');
         INSERT INTO audio_segments
             (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (1, 'usb', '/x.flac', '2026-09-10T12:00:00+00:00',
                 '2026-09-10T12:00:04+00:00', 16000, 1);",
    )
    .expect("schema");
    conn.execute(
        "INSERT INTO transcript_segments
             (audio_segment_id, start_utc, end_utc, text, language, asr_confidence,
              asr_model, speaker_label, speaker_guess, speaker_score, speaker_cluster,
              provenance, loudness, created_utc)
         VALUES (1, '2026-09-10T12:00:00+00:00', '2026-09-10T12:00:04+00:00', ?1,
                 'en', 0.42, 'whisper', ?2, ?3, ?4, 'SPEAKER_01', 'per-mic', 0.03,
                 '2026-09-10T12:00:05+00:00')",
        rusqlite::params![text, speaker, guess.map(|(n, _)| n), guess.map(|(_, s)| s)],
    )
    .expect("turn");
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (id, text),
    )
    .expect("fts");
    id
}

/// Without a session the archive is refused, and the message says what to do
/// rather than showing a status code.
#[test]
fn without_a_session_the_archive_is_refused_and_the_message_says_what_to_do() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), None);

    let err = api.search("marmalade", 10).expect_err("must refuse");
    let said = err.to_string();
    assert!(said.contains("not signed in"), "got: {said}");
    assert!(said.contains("recall_session"), "got: {said}");
}

#[test]
fn a_rejected_session_is_reported_as_rejected_not_as_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some("not-a-real-token".to_owned()));

    let said = api
        .search("marmalade", 10)
        .expect_err("must refuse")
        .to_string();
    assert!(said.contains("session rejected"), "got: {said}");
}

#[test]
fn a_search_with_a_session_finds_the_turn_and_renders_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let hits = api.search("marmalade", 10).expect("search");
    assert_eq!(hits.len(), 1, "one turn matches");
    let line = render::hit(&hits[0]);
    assert!(line.contains("marmalade on the windowsill"), "got: {line}");
    assert!(line.contains("[en]"), "the language is shown: {line}");
    assert!(line.contains("(usb)"), "the source is shown: {line}");
}

/// The attribution rule against a real row: recalld must put the guess and its
/// score where the rendering looks for them.
#[test]
fn an_unconfirmed_guess_arrives_with_its_score_and_is_shown_as_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(
        dir.path(),
        "marmalade on the windowsill",
        None,
        Some(("Pippijn", 0.76)),
    );
    let api = Api::new(&serve(dir.path()), Some(session()));

    let hits = api.search("marmalade", 10).expect("search");
    assert_eq!(render::attribution(&hits[0]), "Pippijn ~76%");
    // The read-through transcript must not assert it.
    assert_eq!(render::who(&hits[0]), "SPEAKER_01");
}

#[test]
fn a_confirmed_name_reaches_both_renderings() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(
        dir.path(),
        "marmalade on the windowsill",
        Some("Pippijn"),
        Some(("Someone Else", 0.9)),
    );
    let api = Api::new(&serve(dir.path()), Some(session()));

    let hits = api.search("marmalade", 10).expect("search");
    assert_eq!(render::attribution(&hits[0]), "Pippijn");
    assert_eq!(render::who(&hits[0]), "Pippijn");
}

#[test]
fn a_turn_can_be_fetched_by_id_and_dumped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let turns = api.transcripts(&[id]).expect("transcripts");
    assert_eq!(turns.len(), 1);
    let dump = render::details(&[id], &turns);
    assert!(dump.contains("status   : visible"), "got:\n{dump}");
    assert!(dump.contains("src=usb"), "got:\n{dump}");
    assert!(dump.contains("(quiet)"), "loudness 0.03 is quiet:\n{dump}");
}

#[test]
fn the_timeline_answers_with_the_newest_turns() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let page = api.timeline(10, None).expect("timeline");
    assert_eq!(page.items.len(), 1);
    assert!(!page.has_more);
}

/// The fixture turn is scored 0.42, under the review threshold.
#[test]
fn the_review_queue_surfaces_a_low_confidence_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let turns = api.review(10).expect("review");
    assert_eq!(turns.len(), 1);
}

/// The correction reaches the corpus, and the old id then answers with the new
/// turn, which is what `render::details`'s supersession note relies on.
#[test]
fn a_correction_is_applied_and_the_old_id_then_answers_with_the_new_turn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let new_id = api
        .correct(id, "marmalade on the window sill")
        .expect("correct");
    assert_ne!(new_id, id, "a correction is a new turn, never an overwrite");

    let turns = api.transcripts(&[id]).expect("transcripts");
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].id, new_id,
        "the old id answers with its replacement"
    );
    assert_eq!(turns[0].text, "marmalade on the window sill");

    let dump = render::details(&[id], &turns);
    assert!(
        dump.contains(&format!("#{id} was superseded by this")),
        "got:\n{dump}"
    );
}

/// `sources` and `capture` are device-exempt, so `recall-cli capture` can check
/// the pause without a session.
#[test]
fn capture_and_sources_answer_without_a_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), None);

    api.capture().expect("capture answers unauthenticated");
    api.sources().expect("sources answers unauthenticated");
}

/// The query string is hand-encoded (`api::urlencode`), so the real router must
/// parse it. Spaces and multi-byte characters are routine in names and phrases.
#[test]
fn a_multi_word_search_term_survives_the_query_string() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let hits = api.search("marmalade windowsill", 10).expect("search");
    assert_eq!(hits.len(), 1, "both words matched the same turn");
}

#[test]
fn a_multi_byte_search_term_survives_the_query_string() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "koffie in het café", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let hits = api.search("café", 10).expect("search");
    assert_eq!(hits.len(), 1, "an accented term reached the server intact");
}

/// Make `usb` an upload source with a second turn, so the session surface has
/// something to list: `/api/sessions` lists uploads only.
fn as_upload_session(root: &std::path::Path, title: &str) {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("db");
    conn.execute(
        "UPDATE sources SET kind = 'upload', name = ?1 WHERE id = 'usb'",
        [title],
    )
    .expect("upload kind");
}

#[test]
fn an_uploaded_session_is_listed_with_its_turn_count() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(
        dir.path(),
        "marmalade on the windowsill",
        Some("Pippijn"),
        None,
    );
    as_upload_session(dir.path(), "Tuesday call");
    let api = Api::new(&serve(dir.path()), Some(session()));

    let items = api.sessions().expect("sessions");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "usb");
    assert_eq!(items[0].title, "Tuesday call");
    assert_eq!(items[0].turn_count, 1);
    assert_eq!(items[0].speakers, vec!["Pippijn".to_owned()]);

    let rendered = render::sessions(&items);
    assert!(
        rendered.contains("Tuesday call") || rendered.contains("usb"),
        "got: {rendered}"
    );
    assert!(rendered.contains("1 turns"), "got: {rendered}");
}

/// Only confirmed names reach the speaker list: on out-of-domain audio a
/// visitor can score high against an enrolled voiceprint.
#[test]
fn a_session_does_not_list_a_guessed_speaker_as_a_participant() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(
        dir.path(),
        "marmalade on the windowsill",
        None,
        Some(("Pippijn", 0.95)),
    );
    as_upload_session(dir.path(), "Tuesday call");
    let api = Api::new(&serve(dir.path()), Some(session()));

    let items = api.sessions().expect("sessions");
    assert_eq!(
        items[0].speakers,
        vec!["unknown".to_owned()],
        "a 0.95 guess is still nobody's confirmed name"
    );
}

#[test]
fn a_session_transcript_reads_through_with_its_speaker() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(
        dir.path(),
        "marmalade on the windowsill",
        Some("Pippijn"),
        None,
    );
    as_upload_session(dir.path(), "Tuesday call");
    let api = Api::new(&serve(dir.path()), Some(session()));

    let export = api.session_transcript("usb").expect("transcript");
    assert_eq!(export.session, "usb");
    assert_eq!(export.turns.len(), 1);
    assert_eq!(export.turns[0].speaker, "Pippijn");

    let rendered = render::export(&export);
    assert!(
        rendered.contains("Pippijn: marmalade on the windowsill"),
        "got:\n{rendered}"
    );
}

/// The day view, through the real folding route: one turn is one conversation,
/// and the card is headed by it.
#[test]
fn a_days_conversations_come_back_folded_and_numbered() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let found = api
        .conversations(
            "2026-09-10T00:00:00+00:00",
            "2026-09-11T00:00:00+00:00",
            300.0,
            200,
        )
        .expect("conversations");
    assert_eq!(found.items.len(), 1);
    assert_eq!(found.items[0].turn_count, 1);

    let listed = render::conversations("2026-09-10", &found.items);
    assert!(listed.contains("1 conversation(s)"), "got:\n{listed}");
    assert!(listed.starts_with("# 2026-09-10"), "got:\n{listed}");

    let read = render::conversation("2026-09-10 · conversation 1", &found.items[0]);
    assert!(read.contains("marmalade on the windowsill"), "got:\n{read}");
}

/// A malformed window is the route's 400, not a dropped filter that would serve
/// the whole archive as one page.
#[test]
fn a_malformed_day_window_is_refused_rather_than_ignored() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let err = api
        .conversations("not-a-date", "2026-09-11T00:00:00+00:00", 300.0, 200)
        .expect_err("must refuse");
    assert!(err.to_string().contains("400"), "got: {err}");
}

/// Add a second turn to the same source, so a substring can be ambiguous.
fn second_turn(root: &std::path::Path, text: &str) -> i64 {
    let conn = rusqlite::Connection::open(root.join("recall.sqlite")).expect("db");
    conn.execute(
        "INSERT INTO transcript_segments
             (audio_segment_id, start_utc, end_utc, text, language, asr_confidence,
              asr_model, speaker_cluster, provenance, loudness, created_utc)
         VALUES (1, '2026-09-10T12:00:10+00:00', '2026-09-10T12:00:14+00:00', ?1,
                 'en', 0.42, 'whisper', 'SPEAKER_01', 'per-mic', 0.03,
                 '2026-09-10T12:00:15+00:00')",
        [text],
    )
    .expect("turn");
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
        (id, text),
    )
    .expect("fts");
    id
}

/// Correcting by the words on screen rather than an id.
#[test]
fn a_correction_can_be_made_by_a_unique_substring_rather_than_an_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = archive(dir.path(), "marmalade on the windowsill", None, None);
    let api = Api::new(&serve(dir.path()), Some(session()));

    let turns = api.source_turns("usb", 1000).expect("source turns");
    let found: Vec<_> = turns
        .iter()
        .filter(|t| t.text.contains("windowsill"))
        .collect();
    assert_eq!(found.len(), 1, "one turn holds the phrase");
    assert_eq!(found[0].id, id);

    let corrected = found[0].text.replace("windowsill", "window sill");
    let new_id = api.correct(found[0].id, &corrected).expect("correct");
    assert_eq!(
        api.transcripts(&[id]).expect("read back")[0].id,
        new_id,
        "the old id answers with its replacement"
    );
}

/// A substring in two turns must not pick one: correcting the wrong turn would
/// undetectably write words onto somebody else's sentence.
#[test]
fn a_substring_in_two_turns_is_ambiguous_and_must_not_be_guessed_at() {
    let dir = tempfile::tempdir().expect("tempdir");
    archive(dir.path(), "marmalade on the windowsill", None, None);
    second_turn(dir.path(), "more marmalade please");
    let api = Api::new(&serve(dir.path()), Some(session()));

    let turns = api.source_turns("usb", 1000).expect("source turns");
    let ambiguous: Vec<_> = turns
        .iter()
        .filter(|t| t.text.contains("marmalade"))
        .collect();
    assert_eq!(ambiguous.len(), 2, "both turns hold it — the CLI must skip");
}

/// `source_turns` must return turns that lost a moment comparison, not only the
/// spine, or a correction reports them as absent.
#[test]
fn every_turn_of_a_source_is_visible_to_a_correction_not_only_the_spine() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = archive(dir.path(), "marmalade on the windowsill", None, None);
    let second = second_turn(dir.path(), "and the kettle is on");
    let api = Api::new(&serve(dir.path()), Some(session()));

    let ids: Vec<i64> = api
        .source_turns("usb", 1000)
        .expect("source turns")
        .iter()
        .map(|t| t.id)
        .collect();
    assert!(
        ids.contains(&first) && ids.contains(&second),
        "got: {ids:?}"
    );
}
