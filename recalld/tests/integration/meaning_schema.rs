//! The migration ladder, held to the database it has to produce.
//!
//! The reference is a dump of the live `recall.sqlite`, not this code's output:
//! a ladder compared with itself is only self-consistent.

use recalld::meaning_schema::{MIGRATIONS, ensure, ensure_to};

/// `type name :: sql`, whitespace-flattened, exactly as the fixture was dumped.
fn schema_of(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT type||' '||name||' :: '||COALESCE(REPLACE(REPLACE(sql,char(10),' '),'  ',' '),'(auto)')
             FROM sqlite_master ORDER BY type, name",
        )
        .expect("read schema");
    stmt.query_map([], |r| r.get::<_, String>(0))
        .expect("rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect")
}

fn fleet_schema() -> Vec<String> {
    include_str!("../fixtures/meaning_schema_fleet.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// The version the fixture was dumped at; later rungs differ by design. Do not
/// re-baseline it from this code's output.
fn fixture_version() -> usize {
    include_str!("../fixtures/meaning_schema_fleet.txt")
        .lines()
        .find_map(|l| l.strip_prefix("# user_version:"))
        .and_then(|v| v.trim().parse().ok())
        .expect("the fixture must record the version it was dumped at")
}

/// Build a database by applying exactly `rungs` migrations.
fn built_to(rungs: usize) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    for (index, statement) in MIGRATIONS.iter().enumerate().take(rungs) {
        conn.execute_batch(statement).expect("rung");
        conn.execute_batch(&format!("PRAGMA user_version = {}", index + 1))
            .expect("stamp");
    }
    conn
}

#[test]
fn the_ladder_builds_the_schema_the_fleet_is_actually_running() {
    let conn = built_to(fixture_version());
    let built = schema_of(&conn);
    let want = fleet_schema();

    // Reported as set differences, which name the object that is wrong.
    let built_set: std::collections::BTreeSet<_> = built.iter().collect();
    let want_set: std::collections::BTreeSet<_> = want.iter().collect();
    let missing: Vec<_> = want_set.difference(&built_set).collect();
    let extra: Vec<_> = built_set.difference(&want_set).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the port does not reproduce the live schema.\n  MISSING (on Isis, not built): {missing:#?}\n  EXTRA (built, not on Isis): {extra:#?}"
    );
}

#[test]
fn the_version_matches_the_rung_count_and_a_second_run_does_nothing() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure(&conn).expect("first");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version as usize, MIGRATIONS.len());

    // `ensure` runs on every start.
    let before = schema_of(&conn);
    ensure(&conn).expect("second");
    assert_eq!(schema_of(&conn), before);
}

#[test]
fn a_database_halfway_up_the_ladder_climbs_the_rest() {
    // Every real upgrade starts partway up: production is never empty.
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    for (index, statement) in MIGRATIONS.iter().enumerate().take(20) {
        conn.execute_batch(statement).expect("early rung");
        conn.execute_batch(&format!("PRAGMA user_version = {}", index + 1))
            .expect("stamp");
    }
    ensure(&conn).expect("climb the rest");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version as usize, MIGRATIONS.len());
    // And it lands on the same schema as climbing from empty.
    let from_empty = rusqlite::Connection::open_in_memory().expect("db");
    ensure(&from_empty).expect("from empty");
    assert_eq!(schema_of(&conn), schema_of(&from_empty));
}

/// A database at v45 holding the spellings the archive has: a mic row's whole
/// second, a room row's `.000000`, a `Z`, nanoseconds, and a local offset.
fn mixed_spellings() -> rusqlite::Connection {
    let conn = built_to(45);
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'USB', 'mic'), ('room', 'Room', 'derived');
         INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels) VALUES
             (1, 'usb', 'a', '2026-09-03T10:00:00+00:00', '2026-09-03T10:01:00.250000+00:00', 16000, 1),
             (2, 'room', 'b', '2026-09-03T10:00:00.000000+00:00', '2026-09-03T10:01:00.000000+00:00', 16000, 1);
         INSERT INTO transcript_segments (audio_segment_id, start_utc, end_utc, text, asr_model, created_utc) VALUES
             (1, '2026-09-03T10:00:05Z', '2026-09-03T12:00:07.5+02:00', 'hallo', 'm', '2026-09-22T20:44:37.187123456+00:00');
         INSERT INTO deleted_segments (source_id, start_utc, deleted_utc) VALUES
             ('room', '2026-09-03T10:02:00.000000+00:00', '2026-09-03T11:00:00');",
    )
    .expect("seed");
    conn
}

#[test]
fn every_stored_instant_is_rewritten_to_the_one_spelling() {
    let conn = mixed_spellings();
    // The last rung that still holds the old lines.
    ensure_to(&conn, 59).expect("migrate");
    let column = |sql: &str| -> Vec<String> {
        let mut stmt = conn.prepare(sql).expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("rows")
            .collect::<Result<_, _>>()
            .expect("collect")
    };
    assert_eq!(
        column("SELECT start_utc || ' ' || end_utc FROM audio_segments ORDER BY id"),
        [
            "2026-09-03T10:00:00+00:00 2026-09-03T10:01:00.250000+00:00",
            "2026-09-03T10:00:00+00:00 2026-09-03T10:01:00+00:00",
        ]
    );
    assert_eq!(
        column(
            "SELECT start_utc || ' ' || end_utc || ' ' || created_utc
             FROM retired_20261012_transcript_segments"
        ),
        [
            "2026-09-03T10:00:05+00:00 2026-09-03T10:00:07.500000+00:00 2026-09-22T20:44:37.187123+00:00"
        ]
    );
    assert_eq!(
        column("SELECT start_utc || ' ' || deleted_utc FROM deleted_segments"),
        ["2026-09-03T10:02:00+00:00 2026-09-03T11:00:00+00:00"]
    );
}

#[test]
fn a_value_that_is_not_an_instant_stops_the_migration_and_changes_nothing() {
    let conn = mixed_spellings();
    conn.execute(
        "INSERT INTO vocabulary (term, created_utc) VALUES ('x', 'yesterday')",
        [],
    )
    .expect("seed");
    let refused = ensure(&conn).expect_err("an unreadable instant must not be kept or guessed");
    assert!(refused.to_string().contains("yesterday"), "{refused}");
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .expect("version");
    assert_eq!(version, 45);
    let room: String = conn
        .query_row(
            "SELECT start_utc FROM audio_segments WHERE id = 2",
            [],
            |r| r.get(0),
        )
        .expect("room row");
    assert_eq!(room, "2026-09-03T10:00:00.000000+00:00");
}

/// The stored spelling is enforced by the database, not by each writer's care.
#[test]
fn an_instant_in_another_spelling_is_refused_and_the_stored_one_accepted() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure(&conn).expect("migrate");
    let insert = |at: &str| {
        conn.execute(
            "INSERT INTO vocabulary (term, created_utc) VALUES (?1, ?2)",
            (format!("t{at}"), at),
        )
    };
    for bad in [
        "2026-09-24T10:00:00Z",
        "2026-09-24T11:00:00+01:00",
        "2026-09-24 10:00:00+00:00",
        "2026-09-24T10:00:00.000+00:00",
    ] {
        assert!(insert(bad).is_err(), "{bad} was stored");
    }
    let whole = chrono::DateTime::from_timestamp(1_790_000_000, 0).expect("whole");
    let fraction = chrono::DateTime::from_timestamp(1_790_000_000, 1_000).expect("fraction");
    for good in [whole, fraction] {
        let spelled = audiocore::instant::python_isoformat_utc(good);
        insert(&spelled).unwrap_or_else(|err| panic!("{spelled}: {err}"));
    }
    let moved = conn.execute(
        "UPDATE vocabulary SET created_utc = '2026-09-24T10:00:00Z'",
        [],
    );
    assert!(moved.is_err(), "an update is held to it too");
}

#[test]
fn words_a_person_typed_or_vouched_for_are_marked_on_their_turn() {
    // Rung 50 reads the corrections already filed: a check or a changed text
    // marks the person's turn, a speaker fix (the machine's words) does not.
    // A database at rung 49: the top built, then every rung above 49 taken off
    // (v50 words_checked, v51 sources.language, v52 idx_ts_audio, v53 edits,
    // v54 rendered_lines, v55 its word timings, v56 a print's naming act,
    // v57 live_lines).
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure_to(&conn, 49).expect("at rung 49");
    conn.execute_batch(
        "INSERT INTO transcript_segments (id, start_utc, end_utc, text, asr_model, provenance)
         VALUES (11, '2026-09-19T10:00:00+00:00', '2026-09-19T10:00:02+00:00', 'a', 'human', 'human correction of #1'),
                (12, '2026-09-19T10:00:02+00:00', '2026-09-19T10:00:04+00:00', 'B', 'human', 'human correction of #2'),
                (13, '2026-09-19T10:00:04+00:00', '2026-09-19T10:00:06+00:00', 'c', 'human', 'human correction of #3');
         INSERT INTO transcript_segments (id, start_utc, end_utc, text, asr_model, superseded_by)
         VALUES (1, '2026-09-19T10:00:00+00:00', '2026-09-19T10:00:02+00:00', 'a', 'whisper', 11),
                (2, '2026-09-19T10:00:02+00:00', '2026-09-19T10:00:04+00:00', 'b', 'whisper', 12),
                (3, '2026-09-19T10:00:04+00:00', '2026-09-19T10:00:06+00:00', 'c', 'whisper', 13);
         INSERT INTO corrections (transcript_segment_id, start_utc, end_utc, original_text, corrected_text, created_utc, words_checked)
         VALUES (1, '2026-09-19T10:00:00+00:00', '2026-09-19T10:00:02+00:00', 'a', 'a', '2026-09-26T12:00:00+00:00', 1),
                (2, '2026-09-19T10:00:02+00:00', '2026-09-19T10:00:04+00:00', 'b', 'B', '2026-09-26T12:00:00+00:00', NULL),
                (3, '2026-09-19T10:00:04+00:00', '2026-09-19T10:00:06+00:00', 'c', 'c', '2026-09-26T12:00:00+00:00', NULL);",
    )
    .expect("rows at rung 49");
    ensure_to(&conn, 59).expect("climb");
    let marked: Vec<(i64, Option<i64>)> = conn
        .prepare(
            "SELECT id, words_checked FROM retired_20261012_transcript_segments
             WHERE id > 10 ORDER BY id",
        )
        .expect("prep")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(marked, vec![(11, Some(1)), (12, Some(1)), (13, None)]);
}

#[test]
fn the_python_capture_columns_go_and_an_audio_row_keeps_its_instant_guard() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure_to(&conn, 61).expect("at rung 61");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio');
         INSERT INTO audio_segments
             (source_id, path, start_utc, end_utc, sample_rate, channels, envelope, speech_s)
         VALUES ('usb', '/data/a.flac', '2026-09-19T10:00:00+00:00',
                 '2026-09-19T10:01:00+00:00', 16000, 1, '[0.1]', 3.5);",
    )
    .expect("a row at rung 61");
    ensure(&conn).expect("climb");

    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('audio_segments')")
        .expect("prep")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("columns");
    for gone in [
        "transcribed_utc",
        "mean_volume",
        "envelope",
        "structure",
        "pushed_utc",
        "speech_s",
    ] {
        assert!(!columns.iter().any(|c| c == gone), "{gone} is still there");
    }
    let kept: String = conn
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .expect("the row");
    assert_eq!(kept, "/data/a.flac");

    let insert = |start: &str| {
        conn.execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('usb', ?1, ?2, '2026-09-19T10:05:00+00:00', 16000, 1)",
            (format!("/data/{start}.flac"), start),
        )
    };
    assert!(insert("2026-09-19T10:02:00Z").is_err(), "another spelling");
    insert("2026-09-19T10:02:00+00:00").expect("the stored spelling");
    let moved = conn.execute(
        "UPDATE audio_segments SET end_utc = '2026-09-19T10:03:00Z'",
        [],
    );
    assert!(moved.is_err(), "an update is held to it too");
}

/// v63: a source and a name belong to someone. The names already known keep
/// their ids and prints; a name is unique per owner, and unowned names still
/// collide, so enrolment's `INSERT OR IGNORE` cannot make a second person.
#[test]
fn owners_arrive_without_losing_a_name_or_its_prints() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure_to(&conn, 62).expect("v62");
    conn.execute_batch(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'USB', 'coreaudio');
         INSERT INTO speakers (id, name) VALUES (7, 'Alex');
         INSERT INTO speaker_embeddings (speaker_id, vector, created_utc)
             VALUES (7, '[1.0]', '2026-10-01T10:00:00+00:00');",
    )
    .expect("v62 rows");

    ensure_to(&conn, 63).expect("v63");

    let owner: Option<String> = conn
        .query_row("SELECT owner FROM sources WHERE id = 'usb'", [], |r| {
            r.get(0)
        })
        .expect("source");
    assert_eq!(owner, None, "unclaimed until an operator claims it");
    let kept: (i64, Option<String>) = conn
        .query_row(
            "SELECT id, owner FROM speakers WHERE name = 'Alex'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("speaker");
    assert_eq!(kept, (7, None));
    let prints: i64 = conn
        .query_row(
            "SELECT count(*) FROM speaker_embeddings WHERE speaker_id = 7",
            [],
            |r| r.get(0),
        )
        .expect("prints");
    assert_eq!(prints, 1);
    let broken: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .expect("fk check");
    assert_eq!(broken, 0);
    // The rebuilt prints point at `speakers`, and still refuse a bad instant.
    let prints_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'speaker_embeddings'",
            [],
            |r| r.get(0),
        )
        .expect("prints sql");
    assert!(
        prints_sql.contains("REFERENCES \"speakers\"")
            || prints_sql.contains("REFERENCES speakers("),
        "{prints_sql}"
    );
    assert!(
        conn.execute(
            "INSERT INTO speaker_embeddings (speaker_id, vector, created_utc) VALUES (7, '[1.0]', 'yesterday')",
            [],
        )
        .is_err()
    );

    // Unowned still collides: the ignore keeps one Alex.
    conn.execute("INSERT OR IGNORE INTO speakers (name) VALUES ('Alex')", [])
        .expect("ignored");
    // Two owners may each know an Alex; one owner may not know two.
    conn.execute(
        "INSERT INTO speakers (owner, name) VALUES ('a', 'Alex')",
        [],
    )
    .expect("a's Alex");
    conn.execute(
        "INSERT INTO speakers (owner, name) VALUES ('b', 'Alex')",
        [],
    )
    .expect("b's Alex");
    assert!(
        conn.execute(
            "INSERT INTO speakers (owner, name) VALUES ('a', 'Alex')",
            []
        )
        .is_err()
    );
    let alexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM speakers WHERE name = 'Alex'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(alexes, 3);
}
