//! The migration ladder, held to the database it has to produce.
//!
//! ⚠ The reference is a dump of the live `recall.sqlite` on isis, not this code's
//! output: a ladder compared against itself is only self-consistent, and what can
//! go wrong is a column spelled differently from the one the archive uses.

use recalld::meaning_schema::{MIGRATIONS, ensure};

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

/// The version the fixture was dumped at.
///
/// ⚠ The comparison runs to this rung, not to the top of the ladder: later
/// migrations differ from the dump by design. Re-baselining the fixture from this
/// code's output would turn a production dump into a self-comparison.
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

    // `ensure` runs on every start; re-running a shipped step would CREATE an
    // existing table.
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
    ensure(&conn).expect("migrate");
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
        column("SELECT start_utc || ' ' || end_utc || ' ' || created_utc FROM transcript_segments"),
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
    // A database one rung short: the top built, then its last rung taken off.
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure(&conn).expect("schema");
    conn.execute_batch(
        "ALTER TABLE transcript_segments DROP COLUMN words_checked;
         PRAGMA user_version = 49;",
    )
    .expect("one rung short");
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
    ensure(&conn).expect("climb");
    let marked: Vec<(i64, Option<i64>)> = conn
        .prepare("SELECT id, words_checked FROM transcript_segments WHERE id > 10 ORDER BY id")
        .expect("prep")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(marked, vec![(11, Some(1)), (12, Some(1)), (13, None)]);
}
