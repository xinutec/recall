use recalld::turn_store::{
    HUMAN_MODEL, HUMAN_OWNED, HiddenKind, HiddenReason, LIVE_MODEL, Provenance, Stage,
    is_human_owned,
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
    let writes = [
        "INSERT INTO transcript_segments",
        "INSERT OR REPLACE INTO transcript_segments",
        "UPDATE transcript_segments",
        "DELETE FROM transcript_segments",
        "INSERT INTO transcript_fts",
    ];
    let mut offenders = Vec::new();
    for (module, statements) in recalld::sql::ALL {
        if *module == "turn_store" {
            continue;
        }
        for sql in *statements {
            let text = sql.text().split_whitespace().collect::<Vec<_>>().join(" ");
            if writes.iter().any(|w| text.contains(w)) {
                offenders.push(format!("{module}: {text}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "write turns through turn_store: {offenders:?}"
    );
}

/// Stored rows carry these spellings, and taking a hide back matches on them.
#[test]
fn every_hidden_reason_keeps_its_stored_spelling() {
    let all = [
        (HiddenReason::LiveReconciled, "live-reconciled"),
        (HiddenReason::CoveredByRoom, "covered by the room stream"),
        (
            HiddenReason::DiarizedBy("per-mic runner".into()),
            "diarized (per-mic runner)",
        ),
        (HiddenReason::SplitInto(39916), "split into pieces (39916)"),
        (HiddenReason::NobodySpoke, "nobody spoke"),
        (HiddenReason::SilentMinute, "silent minute"),
        (HiddenReason::SetAside, "set aside for re-transcription"),
        (
            HiddenReason::RetranscriptionUndone,
            "re-transcription undone",
        ),
        (HiddenReason::SecondCopy, "a second copy of the same minute"),
    ];
    for (reason, stored) in all {
        // A new reason fails to compile here until it is listed above.
        match reason {
            HiddenReason::LiveReconciled
            | HiddenReason::CoveredByRoom
            | HiddenReason::DiarizedBy(_)
            | HiddenReason::SplitInto(_)
            | HiddenReason::NobodySpoke
            | HiddenReason::SilentMinute
            | HiddenReason::SetAside
            | HiddenReason::RetranscriptionUndone
            | HiddenReason::SecondCopy => {}
        }
        assert_eq!(reason.to_string(), stored);
        assert_eq!(HiddenKind::of(stored), reason.kind(), "{stored}");
    }
}

/// Spellings no current writer produces read as `Other`, never as a kind a
/// person could take back.
#[test]
fn an_older_spelling_is_another_kind() {
    for stored in [
        "reprocessed (mlx-community/whisper-large-v3-turbo)",
        "superseded by sync push",
        "no speech in span",
        "nobody spoke!",
        "diarized",
        "",
    ] {
        assert_eq!(HiddenKind::of(stored), HiddenKind::Other, "{stored:?}");
    }
}
