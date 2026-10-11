//! The labelling reads. Each rule's failure would be quiet: a list still
//! renders, a clip still plays.

use recalld::labels::{
    correction_window, corrections_by_speaker, known_speaker_names, list_corrections,
};
use rusqlite::Connection;

fn schema(conn: &Connection) {
    recalld::meaning_schema::ensure(conn).expect("schema");
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    conn
}

/// A person's naming of four seconds of clip 1, starting `second` in; `enrol`
/// false is "keep the name, not the voiceprint". Returns its id.
fn label(conn: &Connection, second: i64, speaker: &str, enrol: bool) -> i64 {
    let at =
        |s: i64| transcript::Instant::from_micros(1_781_460_000_000_000 + s * 1_000_000).unwrap();
    recalld::edit_log::append(
        conn,
        at(100),
        &transcript::Act::Speaker {
            clip: transcript::ClipId::from_stored(1),
            span: transcript::Span::new(at(second), at(second + 4)).unwrap(),
            name: transcript::Name::new(speaker).unwrap(),
            enrol,
        },
        None,
    )
    .expect("label")
    .stored()
}

#[test]
fn the_roster_never_offers_a_diarization_cluster_tag_as_a_name() {
    // A diarization tag is not a person to offer.
    let conn = db();
    conn.execute("INSERT INTO speakers (id, name) VALUES (1, 'Oskar')", ())
        .expect("speaker");
    conn.execute(
        "INSERT INTO rendered_lines (id, start_utc, end_utc, text, asr_model, speaker_label, tier)
         VALUES (1, '2026-06-14T18:00:00+00:00', '2026-06-14T18:00:01+00:00', 'x', 'whisper', 'SPEAKER_01', 'diarized'),
                (2, '2026-06-14T18:00:01+00:00', '2026-06-14T18:00:02+00:00', 'y', 'whisper', 'Dr Lee', 'diarized')",
        (),
    )
    .expect("turns");

    let names = known_speaker_names(&conn).expect("roster").names;

    assert!(names.contains(&"Oskar".to_string()));
    assert!(names.contains(&"Dr Lee".to_string()));
    assert!(
        !names.iter().any(|n| n.starts_with("SPEAKER")),
        "got {names:?}"
    );
}

#[test]
fn the_roster_is_case_insensitively_ordered_and_free_of_blanks() {
    let conn = db();
    conn.execute(
        "INSERT INTO speakers (id, name) VALUES (1, 'zoe'), (2, 'Alex'), (3, '')",
        (),
    )
    .expect("speakers");

    let names = known_speaker_names(&conn).expect("roster").names;

    assert_eq!(names, vec!["Alex", "zoe"]);
}

#[test]
fn a_label_kept_from_the_voiceprints_stays_out_of_the_list() {
    // A person judged the clip unusable; it is not offered to review.
    let conn = db();
    let kept = label(&conn, 0, "Oskar", true);
    label(&conn, 10, "Oskar", false);

    let items = list_corrections(&conn, None, 200).expect("list");

    assert_eq!(items.iter().map(|l| l.id).collect::<Vec<_>>(), [kept]);
}

#[test]
fn corrections_are_newest_first_and_filterable_by_voice() {
    let conn = db();
    let a = label(&conn, 0, "Alex", true);
    let b = label(&conn, 10, "Oskar", true);
    let c = label(&conn, 20, "Alex", true);

    let all = list_corrections(&conn, None, 200).expect("all");
    assert_eq!(
        all.iter().map(|l| l.id).collect::<Vec<_>>(),
        vec![c, b, a],
        "newest first"
    );

    let alex = list_corrections(&conn, Some("Alex"), 200).expect("filtered");
    assert_eq!(alex.iter().map(|l| l.id).collect::<Vec<_>>(), vec![c, a]);
}

#[test]
fn each_label_carries_the_url_that_plays_it() {
    let conn = db();
    let id = label(&conn, 0, "Oskar", true);

    let items = list_corrections(&conn, None, 200).expect("list");

    assert_eq!(items[0].audio_url, format!("/api/correction/{id}/audio"));
}

#[test]
fn the_per_speaker_tally_counts_what_the_list_shows() {
    let conn = db();
    label(&conn, 0, "Alex", true);
    label(&conn, 10, "Alex", false);
    label(&conn, 20, "Oskar", true);

    let tally = corrections_by_speaker(&conn).expect("tally");

    assert_eq!(tally.get("Alex"), Some(&1));
    assert_eq!(tally.get("Oskar"), Some(&1));
}

#[test]
fn a_label_plays_its_exact_cut_by_default_and_pads_only_on_request() {
    // Exact by default: the Labels page audits the cut. Context is opt-in.
    let (exact_start, exact_end) = correction_window(10.0, 12.0, false);
    assert!((exact_start - 10.0).abs() < 1e-9);
    assert!((exact_end - 12.0).abs() < 1e-9);

    let (padded_start, padded_end) = correction_window(10.0, 12.0, true);
    assert!(padded_start < 10.0 && padded_end > 12.0);
    assert!((padded_end - padded_start) >= 5.0, "context has a floor");
}

#[test]
fn an_exact_window_never_starts_before_the_file() {
    // At the start of a recording: ffmpeg fails on a negative -ss.
    let (start, _) = correction_window(-0.4, 1.0, false);
    assert!(start >= 0.0);
}
