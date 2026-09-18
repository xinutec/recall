//! The ported migration ladder, held to the database it has to produce.
//!
//! ⚠ **The reference comes from ISIS, not from this code.** A ladder compared
//! against its own output proves only that it is self-consistent; what can
//! actually go wrong is the port spelling a column differently from the column
//! 145,000 turns are stored in. So the fixture is a dump of the live
//! `recall.sqlite` and the test builds a database from nothing and diffs.

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

#[test]
fn the_ladder_builds_the_schema_the_fleet_is_actually_running() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    ensure(&conn).expect("migrate from empty");

    let built = schema_of(&conn);
    let want = fleet_schema();

    // Reported as set differences, because "line 14 differs" is unreadable for a
    // schema and the useful question is always WHICH object is wrong.
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

    // ⚠ Idempotence is not a nicety here: `ensure` runs on every start, and a
    // ladder that re-ran a shipped step would try to CREATE an existing table.
    let before = schema_of(&conn);
    ensure(&conn).expect("second");
    assert_eq!(schema_of(&conn), before);
}

#[test]
fn a_database_halfway_up_the_ladder_climbs_the_rest() {
    // The shape every real upgrade takes, and the one an all-or-nothing check
    // cannot see: production is never empty.
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
    assert_eq!(schema_of(&conn), fleet_schema());
}
