//! Every declared statement against the schema it names (`recalld::sql`).

use recalld::sql::{ALL, Db};

/// Both databases as production opens and migrates them.
fn migrated(dir: &std::path::Path) -> (rusqlite::Connection, rusqlite::Connection) {
    let ingest = recalld::store::open(dir).expect("ingest");
    let meaning = recalld::work::open_write(dir).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("migrate");
    (meaning, ingest)
}

#[test]
fn every_statement_prepares_against_the_migrated_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (meaning, ingest) = migrated(dir.path());
    let mut refused = Vec::new();
    let mut checked = 0;
    for (module, statements) in ALL {
        for sql in *statements {
            let conn = match sql.db() {
                Db::Meaning => &meaning,
                Db::Ingest => &ingest,
            };
            checked += 1;
            if let Err(err) = sql.prepare(conn) {
                let head = sql.text().split_whitespace().take(8).collect::<Vec<_>>();
                refused.push(format!("{module}: {err} — {}", head.join(" ")));
            }
        }
    }
    assert!(refused.is_empty(), "{}", refused.join("\n"));
    assert!(checked > 100, "only {checked} statements declared");
}

/// A module whose block is missing from `ALL` would go unchecked.
#[test]
fn every_module_with_statements_is_in_the_registry() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut declaring = Vec::new();
    for entry in std::fs::read_dir(&src).expect("src") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("crate::statements! {") {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            declaring.push(stem.to_owned());
        }
    }
    declaring.sort();
    let mut listed: Vec<String> = ALL.iter().map(|(m, _)| (*m).to_owned()).collect();
    listed.sort();
    assert_eq!(declaring, listed);
}

/// A `Sql` built by hand would run without being in any block.
#[test]
fn a_statement_is_built_only_by_the_macro() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&src).expect("src") {
        let path = entry.expect("entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if name != "sql.rs" && text.contains("Sql::declared") {
            offenders.push(name.to_owned());
        }
    }
    assert!(offenders.is_empty(), "{offenders:?}");
}

/// Rendering a clip reads its old rows for their ids, on every refresh of every
/// clip: by the index, never a scan of every rendered line.
#[test]
fn reading_a_clips_rendered_rows_does_not_scan_the_view() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (meaning, _ingest) = migrated(dir.path());
    let rows = ALL
        .iter()
        .filter(|(module, _)| *module == "view")
        .flat_map(|(_, statements)| statements.iter())
        .find(|sql| {
            sql.text()
                .contains("FROM rendered_lines WHERE clip_id = ?1")
        })
        .expect("the view's per-clip read");
    let mut plan = meaning
        .prepare(&format!("EXPLAIN QUERY PLAN {}", rows.text()))
        .expect("plan");
    let steps: Vec<String> = plan
        .query_map([0], |r| r.get::<_, String>(3))
        .expect("steps")
        .collect::<Result<_, _>>()
        .expect("read");
    assert!(
        !steps.iter().any(|s| s.starts_with("SCAN rendered_lines")),
        "{steps:?}"
    );
}
