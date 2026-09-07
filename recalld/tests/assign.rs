//! Assigning a span to a speaker — the drag-select gesture, and the only surgery
//! in the product that creates turns.

use chrono::{DateTime, Duration, Utc};
use recalld::assign::{Piece, Span, Turn, Word, assign_span, min_width, pieces_of};
use rusqlite::Connection;

const NOW: &str = "2026-09-07T12:00:00+00:00";

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-03T09:51:00+00:00")
        .expect("base")
        .with_timezone(&Utc)
        + Duration::seconds(seconds)
}

fn turn(text: &str, words: Option<Vec<Word>>) -> Turn {
    Turn {
        id: 1,
        audio_segment_id: Some(7),
        start: at(0),
        end: at(10),
        text: text.to_owned(),
        language: Some("en".into()),
        language_confidence: Some(0.9),
        asr_confidence: Some(0.8),
        asr_model: Some("mlx-whisper".into()),
        speaker_label: None,
        speaker_cluster: Some("SPEAKER_00".into()),
        provenance: Some("diarized (mlx-whisper)".into()),
        words,
    }
}

fn word(s: f64, e: f64, w: &str) -> Word {
    Word {
        s,
        e,
        w: w.to_owned(),
    }
}

fn texts(pieces: &[Piece]) -> Vec<String> {
    pieces.iter().map(|p| p.text.clone()).collect()
}

#[test]
fn a_cut_never_bisects_a_word() {
    let t = turn("a list of errands and we want to", None);
    // 6 is inside "list"; it snaps to the nearer space.
    let pieces = pieces_of(&t, &[6], &[None, Some("Dr Lee".into())]);

    assert_eq!(texts(&pieces), ["a list", "of errands and we want to"]);
}

#[test]
fn a_cut_inside_a_word_ties_to_the_left() {
    // "abc def": offset 5 is one from each space. The Python takes the left on a
    // tie (`left if at - left <= right - at`), and a port that took the right
    // would move every ambiguous cut by a word.
    let t = turn("abc def ghi", None);

    let pieces = pieces_of(&t, &[5], &[None, Some("Dr Lee".into())]);

    assert_eq!(texts(&pieces), ["abc", "def ghi"]);
}

#[test]
fn an_accented_turn_cuts_by_character_not_by_byte() {
    // ⚠ THE PORT TRAP. "geëvalueerd" is 11 characters and 12 bytes. Rust's &str
    // indexes by byte, so byte-based arithmetic would cut in the wrong place —
    // and landing inside the ë would PANIC rather than misbehave quietly.
    let t = turn("wij hebben dat geëvalueerd vandaag", None);

    // Character 15 is the start of "geëvalueerd".
    let pieces = pieces_of(&t, &[15], &[None, Some("Dr Lee".into())]);

    assert_eq!(texts(&pieces), ["wij hebben dat", "geëvalueerd vandaag"]);
}

#[test]
fn a_turn_of_only_accented_words_survives_a_cut_at_every_offset() {
    // A fuzz of the same trap: no offset may panic, and every piece must still
    // be a substring of the original.
    let text = "één twee drie vière vijf zés";
    let t = turn(text, None);
    let count = text.chars().count();

    for cut in 0..=count {
        let pieces = pieces_of(&t, &[cut], &[None, Some("X".into())]);
        for piece in &pieces {
            assert!(
                text.contains(piece.text.as_str()),
                "cut {cut} produced {:?}, not a substring",
                piece.text
            );
        }
    }
}

#[test]
fn word_timings_place_the_cut_at_the_words_own_time() {
    // With timings the split time is the word's, not a character fraction — the
    // whole reason timings are stored.
    let words = vec![
        word(0.0, 1.0, "one"),
        word(1.0, 2.0, " two"),
        word(6.0, 7.0, " three"),
    ];
    let t = turn("one two three", Some(words));

    let pieces = pieces_of(&t, &[7], &[None, Some("Dr Lee".into())]);

    assert_eq!(texts(&pieces), ["one two", "three"]);
    // "three" starts at 6.0s into the turn, not at 7/13 of its span.
    assert_eq!(pieces[1].start, at(0) + Duration::seconds(6));
}

#[test]
fn the_turns_own_edges_are_kept_exact_not_snapped_to_a_word() {
    // ⚠ A word's timestamp can sit inside leading silence or drift. Anchoring the
    // first piece to it would drop the turn's opening audio.
    let words = vec![word(2.5, 3.0, "late"), word(3.0, 4.0, " start")];
    let t = turn("late start", Some(words));

    let pieces = pieces_of(&t, &[4], &[None, Some("X".into())]);

    assert_eq!(pieces[0].start, t.start, "not 2.5s in");
    assert_eq!(pieces.last().expect("piece").end, t.end);
}

#[test]
fn each_piece_carries_its_own_words_rebased_to_its_start() {
    let words = vec![
        word(0.0, 1.0, "one"),
        word(1.0, 2.0, " two"),
        word(6.0, 7.0, " three"),
    ];
    let t = turn("one two three", Some(words));

    let pieces = pieces_of(&t, &[7], &[None, Some("X".into())]);

    let tail = pieces[1].words.as_ref().expect("the tail keeps its word");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].w, " three");
    assert!(
        (tail[0].s - 0.0).abs() < 1e-9,
        "rebased to the piece's start, got {}",
        tail[0].s
    );
}

#[test]
fn an_empty_piece_drops_out_rather_than_becoming_a_blank_turn() {
    let t = turn("hello world", None);

    // A cut at the very start yields nothing to the left of it.
    let pieces = pieces_of(&t, &[0], &[Some("A".into()), Some("B".into())]);

    assert_eq!(texts(&pieces), ["hello world"]);
    assert_eq!(pieces[0].speaker.as_deref(), Some("B"));
}

#[test]
fn a_collapsed_cut_is_widened_to_a_playable_span() {
    // Two words that aligned to the same instant would otherwise make a
    // zero-length, audio-less turn.
    let pieces = vec![
        Piece {
            start: at(0),
            end: at(0),
            text: "a".into(),
            speaker: None,
            words: None,
        },
        Piece {
            start: at(0),
            end: at(10),
            text: "b".into(),
            speaker: None,
            words: None,
        },
    ];

    let widened = min_width(pieces, at(0), at(10));

    assert!(widened[0].end > widened[0].start, "piece 0 has no width");
    assert!(
        widened[1].start >= widened[0].end,
        "pieces must stay ordered"
    );
    assert!(
        widened.iter().all(|p| p.end <= at(10)),
        "clamped to the turn"
    );
}

// --- against the database ----------------------------------------------------

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    conn.execute_batch(
        "CREATE TABLE sources (id TEXT PRIMARY KEY, name TEXT, kind TEXT);
         CREATE TABLE audio_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT NOT NULL,
            start_utc TEXT, end_utc TEXT);
         CREATE TABLE transcript_segments (
            id INTEGER PRIMARY KEY AUTOINCREMENT, audio_segment_id INTEGER,
            start_utc TEXT NOT NULL, end_utc TEXT NOT NULL, text TEXT NOT NULL,
            language TEXT, language_confidence REAL, asr_confidence REAL,
            asr_model TEXT, speaker_label TEXT, speaker_cluster TEXT,
            provenance TEXT, created_utc TEXT, word_timings TEXT,
            superseded_by INTEGER, hidden_reason TEXT);
         CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');
         INSERT INTO sources (id, name, kind) VALUES ('m', 'm', 'upload');
         INSERT INTO audio_segments (id, source_id) VALUES (7, 'm');",
    )
    .expect("schema");
    conn
}

fn add(conn: &Connection, id: i64, start: i64, text: &str, speaker: Option<&str>) {
    conn.execute(
        "INSERT INTO transcript_segments
             (id, audio_segment_id, start_utc, end_utc, text, asr_model, speaker_label,
              provenance)
         VALUES (?1, 7, ?2, ?3, ?4, 'mlx-whisper', ?5, 'diarized (mlx-whisper)')",
        rusqlite::params![
            id,
            at(start).to_rfc3339(),
            at(start + 10).to_rfc3339(),
            text,
            speaker
        ],
    )
    .expect("turn");
}

fn current(conn: &Connection) -> Vec<(i64, String, Option<String>)> {
    let mut stmt = conn
        .prepare(
            "SELECT id, text, speaker_label FROM transcript_segments
             WHERE superseded_by IS NULL AND hidden_reason IS NULL ORDER BY start_utc, id",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

#[test]
fn assigning_a_whole_turn_relabels_it_without_splitting() {
    let mut conn = db();
    add(&conn, 1, 0, "hello world", None);

    let touched = assign_span(
        &mut conn,
        "m",
        Span {
            start_turn: 1,
            start_char: 0,
            end_turn: 1,
            end_char: 11,
        },
        "Dr Lee",
        NOW,
    )
    .expect("assigned");

    assert_eq!(touched, 1);
    let rows = current(&conn);
    assert_eq!(rows.len(), 1, "no split, so no new turns");
    assert_eq!(rows[0].0, 1, "the SAME row, relabelled in place");
    assert_eq!(rows[0].2.as_deref(), Some("Dr Lee"));
}

#[test]
fn assigning_part_of_a_turn_splits_it_and_hides_the_original() {
    // ⚠ Hidden, never deleted: a wrong split has to be recoverable.
    let mut conn = db();
    add(
        &conn,
        1,
        0,
        "a list of errands and we want to make sure",
        None,
    );

    let touched = assign_span(
        &mut conn,
        "m",
        Span {
            start_turn: 1,
            start_char: 0,
            end_turn: 1,
            end_char: 17,
        },
        "Dr Lee",
        NOW,
    )
    .expect("assigned");

    assert_eq!(touched, 1);
    let rows = current(&conn);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "a list of errands");
    assert_eq!(rows[0].2.as_deref(), Some("Dr Lee"));
    assert_eq!(rows[1].2, None, "the remainder keeps what it had");

    let hidden: Option<String> = conn
        .query_row(
            "SELECT hidden_reason FROM transcript_segments WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .expect("original");
    assert_eq!(hidden.as_deref(), Some("split into pieces (1)"));
}

#[test]
fn a_span_across_turns_splits_both_edges_and_relabels_the_middle() {
    let mut conn = db();
    add(&conn, 1, 0, "first turn here", None);
    add(&conn, 2, 20, "entirely inside the span", None);
    add(&conn, 3, 40, "last turn here", None);

    let touched = assign_span(
        &mut conn,
        "m",
        Span {
            start_turn: 1,
            start_char: 6,
            end_turn: 3,
            end_char: 4,
        },
        "Dr Lee",
        NOW,
    )
    .expect("assigned");

    assert_eq!(touched, 3, "both edges plus the middle");
    let named: Vec<String> = current(&conn)
        .into_iter()
        .filter(|(_, _, s)| s.as_deref() == Some("Dr Lee"))
        .map(|(_, t, _)| t)
        .collect();
    assert!(named.contains(&"turn here".to_owned()), "got {named:?}");
    assert!(
        named.contains(&"entirely inside the span".to_owned()),
        "got {named:?}"
    );
    assert!(named.contains(&"last".to_owned()), "got {named:?}");
}

#[test]
fn a_right_to_left_selection_is_the_same_as_left_to_right() {
    let mut conn = db();
    add(&conn, 1, 0, "first turn here", None);
    add(&conn, 2, 20, "last turn here", None);
    let mut mirror = db();
    add(&mirror, 1, 0, "first turn here", None);
    add(&mirror, 2, 20, "last turn here", None);

    let forward = assign_span(
        &mut conn,
        "m",
        Span {
            start_turn: 1,
            start_char: 6,
            end_turn: 2,
            end_char: 4,
        },
        "Dr Lee",
        NOW,
    )
    .expect("fwd");
    let backward = assign_span(
        &mut mirror,
        "m",
        Span {
            start_turn: 2,
            start_char: 4,
            end_turn: 1,
            end_char: 6,
        },
        "Dr Lee",
        NOW,
    )
    .expect("back");

    assert_eq!(forward, backward);
    assert_eq!(
        current(&conn)
            .iter()
            .map(|r| (r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>(),
        current(&mirror)
            .iter()
            .map(|r| (r.1.clone(), r.2.clone()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_second_assign_on_an_already_split_turn_does_nothing() {
    // ⚠ The double-tap. Both callers read the turn as live; only the one that
    // wins the atomic claim splits it. Without that the second stamps out a
    // duplicate set of pieces.
    let mut conn = db();
    add(&conn, 1, 0, "a list of errands and we want to", None);
    let span = Span {
        start_turn: 1,
        start_char: 0,
        end_turn: 1,
        end_char: 17,
    };

    let first = assign_span(&mut conn, "m", span, "Dr Lee", NOW).expect("first");
    let before = current(&conn).len();
    // The second arrives holding the same now-hidden id.
    let second = assign_span(&mut conn, "m", span, "Dr Lee", NOW).expect("second");

    assert_eq!(first, 1);
    assert_eq!(second, 0, "the stale id must be a no-op");
    assert_eq!(current(&conn).len(), before, "no duplicate pieces");
}

#[test]
fn a_split_of_a_diarized_turn_stays_diarized_quality() {
    let mut conn = db();
    add(&conn, 1, 0, "a list of errands and we want to", None);

    assign_span(
        &mut conn,
        "m",
        Span {
            start_turn: 1,
            start_char: 0,
            end_turn: 1,
            end_char: 17,
        },
        "Dr Lee",
        NOW,
    )
    .expect("assigned");

    let provenance: String = conn
        .query_row(
            "SELECT provenance FROM transcript_segments WHERE hidden_reason IS NULL LIMIT 1",
            [],
            |r| r.get(0),
        )
        .expect("piece");
    assert_eq!(
        provenance, "diarized-aligned split of #1",
        "the aligned marker keeps the pieces out of the re-diarize work-list"
    );
}

#[test]
fn split_pieces_are_findable_by_search() {
    let mut conn = db();
    add(&conn, 1, 0, "vorasidenib dosage and the rest of it", None);

    assign_span(
        &mut conn,
        "m",
        Span {
            start_turn: 1,
            start_char: 0,
            end_turn: 1,
            end_char: 19,
        },
        "Dr Lee",
        NOW,
    )
    .expect("assigned");

    let hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_fts WHERE transcript_fts MATCH 'vorasidenib'",
            [],
            |r| r.get(0),
        )
        .expect("search");
    assert_eq!(hits, 1, "a piece missing from the index is unsearchable");
}

#[test]
fn a_turn_from_another_session_is_refused_rather_than_split() {
    let mut conn = db();
    add(&conn, 1, 0, "belongs to m", None);
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('other', 'o', 'upload')",
        [],
    )
    .expect("source");

    let touched = assign_span(
        &mut conn,
        "other",
        Span {
            start_turn: 1,
            start_char: 0,
            end_turn: 1,
            end_char: 5,
        },
        "Dr Lee",
        NOW,
    )
    .expect("call");

    assert_eq!(touched, 0);
    assert_eq!(current(&conn)[0].2, None, "untouched");
}
