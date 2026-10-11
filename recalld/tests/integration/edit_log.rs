//! The edit log keeps what people did, in order, and reads back exactly that:
//! per clip with its session's acts and their retractions, append-only, seeded
//! once from today's tables.

use recalld::edit_log::{self, LogError};
use rusqlite::Connection;
use transcript::{Act, Clip, ClipId, EditId, Instant, Language, Name, SourceId, Span};

fn meaning() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().expect("tmp");
    let conn = recalld::work::open_write(dir.path()).expect("meaning");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    (dir, conn)
}

fn at(s: i64) -> Instant {
    Instant::from_micros(1_759_000_000_000_000 + s * 1_000_000).expect("in range")
}

fn clip(id: i64, source: &str) -> Clip {
    Clip {
        id: ClipId::from_stored(id),
        source: SourceId::parse(source).expect("source"),
        start: at(0),
        filename: format!("{source}-x.flac"),
        path: format!("ingest/{source}/{source}-x.flac"),
    }
}

fn speaker(clip: i64, who: &str) -> Act {
    Act::Speaker {
        clip: ClipId::from_stored(clip),
        span: Span::new(at(1), at(2)).expect("ordered"),
        name: Name::new(who).expect("name"),
        enrol: true,
    }
}

#[test]
fn a_clip_reads_its_own_acts_its_sessions_and_their_retractions_in_order() {
    let (_dir, conn) = meaning();
    let mine = edit_log::append(&conn, at(10), &speaker(1, "Robin"), None).expect("append");
    edit_log::append(&conn, at(11), &speaker(2, "Sam"), None).expect("append");
    let session = Act::Language {
        source: SourceId::parse("usb").expect("source"),
        language: Language::Dutch,
    };
    edit_log::append(&conn, at(12), &session, None).expect("append");
    let back = Act::Retract { edit: mine };
    edit_log::append(&conn, at(13), &back, None).expect("append");

    let acts: Vec<Act> = edit_log::for_clip(&conn, &clip(1, "usb"))
        .expect("read")
        .into_iter()
        .map(|e| e.act)
        .collect();
    assert_eq!(acts, [speaker(1, "Robin"), session, back]);
    assert_eq!(edit_log::all(&conn).expect("all").len(), 4);
}

#[test]
fn a_retraction_of_nothing_is_refused_and_a_stored_act_cannot_be_changed() {
    let (_dir, conn) = meaning();
    let missing = Act::Retract {
        edit: EditId::from_stored(99),
    };
    assert!(matches!(
        edit_log::append(&conn, at(1), &missing, None),
        Err(LogError::NoTarget(_))
    ));
    edit_log::append(&conn, at(1), &speaker(1, "Robin"), None).expect("append");
    let changed = conn.execute("UPDATE edits SET at_us = 0", []);
    assert!(changed.is_err(), "the log is append-only");
}

#[test]
fn a_row_that_is_not_an_act_stops_the_read() {
    let (_dir, conn) = meaning();
    conn.execute(
        "INSERT INTO edits (at_us, act, clip_id) VALUES (1, '{\"Speaker\":{\"name\":\"\"}}', 1)",
        [],
    )
    .expect("insert");
    assert!(matches!(
        edit_log::for_clip(&conn, &clip(1, "usb")),
        Err(LogError::Unreadable { .. })
    ));
}

#[test]
fn the_seed_fills_an_empty_log_once() {
    let (dir, conn) = meaning();
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    conn.execute(
        "INSERT INTO sources (id, name, kind, language) VALUES ('meeting-20260520-1901', 'm', 'upload', 'nl')",
        [],
    )
    .expect("pinned session");

    assert_eq!(
        edit_log::seed(&conn, &ingest, dir.path(), at(100)).expect("seed"),
        1
    );
    assert_eq!(
        edit_log::seed(&conn, &ingest, dir.path(), at(200)).expect("again"),
        0
    );
    let edits = edit_log::all(&conn).expect("all");
    assert_eq!(edits.len(), 1);
    assert!(matches!(
        edits[0].act,
        Act::Language {
            language: Language::Dutch,
            ..
        }
    ));
    let origin: String = conn
        .query_row("SELECT origin FROM edits", [], |r| r.get(0))
        .expect("origin");
    assert_eq!(origin, "Pin");
}
