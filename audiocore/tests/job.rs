use audiocore::job::Kind;

#[test]
fn every_kind_reads_back_from_its_spelling_and_serialises_to_it() {
    for kind in Kind::ALL {
        assert_eq!(kind.as_str().parse::<Kind>(), Ok(kind));
        assert_eq!(
            serde_json::to_string(&kind).expect("json"),
            format!("\"{}\"", kind.as_str())
        );
    }
    assert!("diarize_segment".parse::<Kind>().is_err());
}

#[test]
fn a_kind_is_a_column() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    conn.execute_batch("CREATE TABLE jobs (kind TEXT)")
        .expect("table");
    conn.execute("INSERT INTO jobs VALUES (?1)", [Kind::EnrollSpeaker])
        .expect("insert");
    let back: Kind = conn
        .query_row("SELECT kind FROM jobs", [], |r| r.get(0))
        .expect("read");
    assert_eq!(back, Kind::EnrollSpeaker);
    conn.execute("INSERT INTO jobs VALUES ('nonsense')", [])
        .expect("insert");
    let bad = conn.query_row("SELECT kind FROM jobs WHERE kind = 'nonsense'", [], |r| {
        r.get::<_, Kind>(0)
    });
    assert!(bad.is_err(), "an unknown kind is an error, not a default");
}
