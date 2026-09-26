//! The playback-clip window, not ffmpeg. A clip that pulls in the neighbouring
//! speaker undoes the diarized attribution, and a clip sliced exactly to a
//! one-second phrase is unlistenable. Both fail silently: the wrong audio plays.

use recalld::audio::{self, Placement, SpanError};
use rusqlite::Connection;
use std::path::PathBuf;

fn schema(conn: &Connection) {
    recalld::meaning_schema::ensure(conn).expect("schema");
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'USB', 'tcp_pcm')",
        [],
    )
    .expect("source");
}

fn recording(conn: &Connection, id: i64, path: &str, start: &str) {
    conn.execute(
        "INSERT INTO audio_segments (id, source_id, path, start_utc, end_utc, sample_rate, channels)
         VALUES (?1, 'usb', ?2, ?3, ?3, 48000, 1)",
        (id, path, start),
    )
    .expect("recording");
}

fn turn(conn: &Connection, id: i64, seg: i64, start: &str, end: &str, extra: &[(&str, &str)]) {
    conn.execute(
        "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc, text, asr_model)
         VALUES (?1, ?2, ?3, ?4, 'x', 'whisper')",
        (id, seg, start, end),
    )
    .expect("turn");
    for (col, val) in extra {
        conn.execute(
            &format!("UPDATE transcript_segments SET {col} = ?1 WHERE id = ?2"),
            (*val, id),
        )
        .expect("extra");
    }
}

fn db() -> Connection {
    let conn = Connection::open_in_memory().expect("open");
    schema(&conn);
    recording(
        &conn,
        1,
        "/archive/usb-20260613T120000.flac",
        "2026-06-13T12:00:00+00:00",
    );
    conn
}

#[test]
fn a_rough_phrase_gets_a_wide_window_so_it_is_listenable() {
    // A whole-phrase turn from Whisper. Sliced exactly, "Ja." is under a second of
    // audio with no lead-in, which is what the padding prevents.
    let conn = db();
    turn(
        &conn,
        10,
        1,
        "2026-06-13T12:00:20+00:00",
        "2026-06-13T12:00:20.500000+00:00",
        &[],
    );

    let p = audio::placement(&conn, 10).expect("query").expect("found");
    assert!(!p.precise);
    let (start, end) = audio::window_for(&p);

    // 0.5s of speech, expanded symmetrically to the 5s floor around its midpoint.
    assert!((end - start - 5.0).abs() < 1e-4, "got {start}..{end}");
    assert!((start - 17.75).abs() < 1e-4, "start {start}");
}

#[test]
fn a_diarized_turn_is_played_tight_so_it_cannot_pull_in_the_next_speaker() {
    // Widening a diarized cutout by the rough turn's 1.5s would drag in whoever
    // spoke next, while the UI still labels the clip with one name.
    let conn = db();
    turn(
        &conn,
        11,
        1,
        "2026-06-13T12:00:20+00:00",
        "2026-06-13T12:00:22+00:00",
        &[("provenance", "diarized-aligned (whisper)")],
    );

    let p = audio::placement(&conn, 11).expect("query").expect("found");
    assert!(p.precise);
    let (start, end) = audio::window_for(&p);

    // 1e-4, not 1e-6: the offset comes from SQLite's `julianday`, a float day
    // count with ~10µs of error, far below the millisecond `-ss` precision.
    assert!((start - 19.8).abs() < 1e-4, "start {start}");
    assert!((end - 22.2).abs() < 1e-4, "end {end}");
}

#[test]
fn word_timings_make_a_turn_precise_even_without_diarized_provenance() {
    // A span-assign split carries word timings but not the diarized marker. It is
    // just as precise a cutout, and must be played just as tight.
    let conn = db();
    turn(
        &conn,
        12,
        1,
        "2026-06-13T12:00:30+00:00",
        "2026-06-13T12:00:31+00:00",
        &[("word_timings", "[[0.0, 1.0, \"ja\"]]")],
    );

    let p = audio::placement(&conn, 12).expect("query").expect("found");
    assert!(p.precise, "word timings alone must make it precise");
}

#[test]
fn a_human_correction_is_not_treated_as_diarized() {
    // A corrected turn's provenance ("human correction of #N") lacks the diarized
    // marker, but its asr_model is `human`, and the tier rules check that first.
    // Reading only provenance would mis-tier every correction.
    let conn = db();
    turn(
        &conn,
        13,
        1,
        "2026-06-13T12:00:40+00:00",
        "2026-06-13T12:00:40.400000+00:00",
        &[
            ("asr_model", "human"),
            ("provenance", "human correction of #12"),
        ],
    );

    let p = audio::placement(&conn, 13).expect("query").expect("found");
    assert!(!p.precise);
}

#[test]
fn the_window_never_starts_before_the_file() {
    // A turn near the very start of a recording: padding would run negative, and a
    // negative -ss makes ffmpeg fail rather than clamp.
    let conn = db();
    turn(
        &conn,
        14,
        1,
        "2026-06-13T12:00:00.200000+00:00",
        "2026-06-13T12:00:00.600000+00:00",
        &[],
    );

    let p = audio::placement(&conn, 14).expect("query").expect("found");
    let (start, _) = audio::window_for(&p);
    assert!(start >= 0.0, "start {start}");
    assert!((start - 0.0).abs() < 1e-9);
}

#[test]
fn a_turn_with_no_audio_segment_is_absent_not_an_error() {
    let conn = db();
    conn.execute(
        "INSERT INTO transcript_segments (id, audio_segment_id, start_utc, end_utc, text, asr_model)
         VALUES (99, NULL, '2026-06-13T12:00:00+00:00', '2026-06-13T12:00:01+00:00', 'x', 'live')",
        (),
    )
    .expect("orphan turn");

    assert_eq!(audio::placement(&conn, 99).expect("query"), None);
    assert_eq!(audio::placement(&conn, 1234).expect("query"), None);
}

#[test]
fn a_span_across_two_recordings_is_refused_rather_than_spliced() {
    // A session stopped and restarted is two files; one window across them would
    // serve unrelated audio under the bubble's label. The UI falls back to
    // per-turn playback.
    let first = Placement {
        path: PathBuf::from("/archive/a.flac"),
        audio_segment_id: 1,
        start_s: 10.0,
        end_s: 12.0,
        precise: true,
    };
    let last = Placement {
        path: PathBuf::from("/archive/b.flac"),
        audio_segment_id: 2,
        start_s: 3.0,
        end_s: 5.0,
        precise: true,
    };

    assert_eq!(
        audio::span_window(&first, &last),
        Err(SpanError::CrossesRecordings)
    );
}

#[test]
fn a_span_within_one_recording_runs_first_start_to_last_end() {
    let first = Placement {
        path: PathBuf::from("/archive/a.flac"),
        audio_segment_id: 7,
        start_s: 10.0,
        end_s: 12.0,
        precise: true,
    };
    let last = Placement {
        path: PathBuf::from("/archive/a.flac"),
        audio_segment_id: 7,
        start_s: 18.0,
        end_s: 20.0,
        precise: true,
    };

    let (start, end) = audio::span_window(&first, &last).expect("same recording");
    // Tight, and spanning the whole run — not just the first turn.
    assert!((start - 9.8).abs() < 1e-6, "start {start}");
    assert!((end - 20.2).abs() < 1e-6, "end {end}");
}

#[test]
fn clip_window_expands_about_the_midpoint_not_the_start() {
    // Expanding to the minimum by pushing only the end would leave a short turn
    // with no lead-in.
    let (start, end) = audio::clip_window(100.0, 101.0, 0.0, 10.0);
    assert!((start - 95.5).abs() < 1e-6, "start {start}");
    assert!((end - 105.5).abs() < 1e-6, "end {end}");
}

#[test]
fn a_caller_padding_widens_a_tight_turn_for_checking_its_words() {
    // Whisper's segment end is often early; played tight, the last word is cut.
    let p = Placement {
        path: PathBuf::from("/x.opus"),
        audio_segment_id: 1,
        start_s: 10.0,
        end_s: 13.0,
        precise: true,
    };
    assert_eq!(audio::padded_window(&p, None), audio::window_for(&p));
    let (start, end) = audio::padded_window(&p, Some(1.0));
    assert!(
        (start - 9.0).abs() < 1e-9 && (end - 14.0).abs() < 1e-9,
        "{start}..{end}"
    );
    // Capped, and never before the file.
    let (start, end) = audio::padded_window(&p, Some(60.0));
    assert!(
        (start - 7.0).abs() < 1e-9 && (end - 16.0).abs() < 1e-9,
        "{start}..{end}"
    );
    assert_eq!(
        audio::padded_window(&p, Some(f64::NAN)),
        audio::window_for(&p)
    );
}
