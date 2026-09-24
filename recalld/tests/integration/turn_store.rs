use recalld::turn_store::{
    HUMAN_MODEL, HUMAN_OWNED, LIVE_MODEL, Provenance, Stage, is_human_owned,
};
#[test]
fn every_provenance_reads_back_as_written() {
    let all = [
        Provenance::Model("mlx-community/whisper-large-v3-turbo".into()),
        Provenance::Model("/Volumes/Backup/recall/adapter-current".into()),
        Provenance::PerMic,
        Provenance::Room,
        Provenance::Diarized("mlx-community/whisper-large-v3-turbo".into()),
        Provenance::DiarizedAligned("per-mic runner".into()),
        Provenance::DiarizedSplit(26899),
        Provenance::Split(39916),
        Provenance::Correction(12),
    ];
    for p in all {
        assert_eq!(p.to_string().parse::<Provenance>().as_ref(), Ok(&p), "{p}");
    }
}

#[test]
fn a_spelling_no_writer_produces_is_refused() {
    for raw in [
        "",
        "diarized",
        "split of #x",
        "per-mic runner",
        "diarized (x",
    ] {
        assert!(raw.parse::<Provenance>().is_err(), "{raw:?}");
    }
}

#[test]
fn the_stage_counts_a_turn_named_in_place_as_diarized() {
    let per_mic = Some(&Provenance::PerMic);
    let model = Some("mlx-community/whisper-large-v3-turbo");
    assert_eq!(Stage::of(model, per_mic, false), Stage::Transcribed);
    assert_eq!(Stage::of(model, per_mic, true), Stage::Diarized);
    assert_eq!(
        Stage::of(Some(HUMAN_MODEL), per_mic, true),
        Stage::Corrected
    );
    assert_eq!(Stage::of(Some(LIVE_MODEL), None, false), Stage::Live);
    let aligned = Provenance::DiarizedAligned("per-mic runner".into());
    assert_eq!(Stage::of(model, Some(&aligned), false), Stage::Diarized);
}

#[test]
fn a_person_owns_what_the_predicate_says() {
    let conn = rusqlite::Connection::open_in_memory().expect("db");
    conn.execute_batch(
        "CREATE TABLE transcript_segments (id INTEGER, asr_model TEXT, speaker_label TEXT);
         INSERT INTO transcript_segments VALUES
           (1, 'human', NULL), (2, 'm', 'Alex'), (3, 'm', NULL), (4, NULL, NULL),
           (5, 'live', 'Alex'), (6, NULL, 'Alex');",
    )
    .expect("rows");
    let sql =
        format!("SELECT id, asr_model, speaker_label, {HUMAN_OWNED} FROM transcript_segments");
    let mut stmt = conn.prepare(&sql).expect("prepare");
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<bool>>(3)?.unwrap_or(false),
            ))
        })
        .expect("query");
    for row in rows {
        let (id, model, label, sql_says) = row.expect("row");
        assert_eq!(
            sql_says,
            is_human_owned(model.as_deref(), label.as_deref()),
            "row {id}"
        );
    }
}

#[test]
fn the_turn_table_has_one_writer() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let writes = [
        "INSERT INTO transcript_segments",
        "INSERT OR REPLACE INTO transcript_segments",
        "UPDATE transcript_segments",
        "DELETE FROM transcript_segments",
        "INSERT INTO transcript_fts",
    ];
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&src).expect("src") {
        let path = entry.expect("entry").path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // The schema's migrations rewrite the table by design.
        if name == "turn_store.rs" || name == "meaning_schema.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        // Comments dropped and whitespace squashed, so SQL split over lines
        // still matches.
        let code = text
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .flat_map(str::split_whitespace)
            .filter(|token| *token != "\\")
            .collect::<Vec<_>>()
            .join(" ");
        if writes.iter().any(|w| code.contains(w)) {
            offenders.push(name.to_owned());
        }
    }
    assert!(
        offenders.is_empty(),
        "write turns through turn_store: {offenders:?}"
    );
}
