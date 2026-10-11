//! The registrar: a microphone clip gets the audio row its lines hang from,
//! with the duration it actually has.

use recalld::ledger::PassKind;
use recalld::registrar::register_segments;

const NOW: &str = "2026-09-11T12:00:00+00:00";

/// A blob and a real audio file: the pass decodes it.
fn ingest_blob(root: &std::path::Path, source: &str, stamp: &str, seconds: f64) -> String {
    let filename = format!("{source}-{stamp}.flac");
    let dir = recalld::store::source_dir(root, source);
    std::fs::create_dir_all(&dir).expect("dir");
    let path = dir.join(&filename);
    let status = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-ar", "48000", "-ac", "1", "-y"])
        .arg(&path)
        .status()
        .expect("ffmpeg");
    assert!(status.success(), "ffmpeg could not write the fixture");
    let conn = recalld::store::open(root).expect("ingest");
    conn.execute(
        "INSERT OR IGNORE INTO segments
             (filename, source, start_utc, bytes, sha256, received_utc)
         VALUES (?1, ?2, ?3, 1, 'x', ?3)",
        (
            &filename,
            source,
            format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15]
            ),
        ),
    )
    .expect("segment");
    filename
}

/// A second file for the same minute in another container, as a phone that
/// uploads both a `.wav` and its transcode leaves.
fn twin_blob(root: &std::path::Path, source: &str, stamp: &str, seconds: f64, ext: &str) -> String {
    let filename = format!("{source}-{stamp}.{ext}");
    let dir = recalld::store::source_dir(root, source);
    let path = dir.join(&filename);
    let status = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-ar", "48000", "-ac", "1", "-y"])
        .arg(&path)
        .status()
        .expect("ffmpeg");
    assert!(status.success(), "ffmpeg could not write the twin");
    let conn = recalld::store::open(root).expect("ingest");
    conn.execute(
        "INSERT OR IGNORE INTO segments
             (filename, source, start_utc, bytes, sha256, received_utc)
         VALUES (?1, ?2, ?3, 1, 'x', ?3)",
        (
            &filename,
            source,
            format!(
                "{}-{}-{}T{}:{}:{}Z",
                &stamp[0..4],
                &stamp[4..6],
                &stamp[6..8],
                &stamp[9..11],
                &stamp[11..13],
                &stamp[13..15]
            ),
        ),
    )
    .expect("row");
    filename
}

/// The meaning plane, with `usb` known as a microphone.
fn meaning_for_registration() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    recalld::meaning_schema::ensure(&conn).expect("schema");
    conn.execute(
        "INSERT INTO sources (id, name, kind) VALUES ('usb', 'usb', 'coreaudio')",
        [],
    )
    .expect("usb");
    conn
}

#[test]
fn a_line_names_the_model_the_shim_will_actually_load() {
    // The shim loads its own default; `SHIM_MODEL` must match `asr.py`.
    let asr = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/recall/asr.py"),
    )
    .expect("asr.py");
    let line = asr
        .lines()
        .find(|l| l.starts_with("DEFAULT_MODEL"))
        .expect("DEFAULT_MODEL is gone from asr.py — this coupling needs rethinking");
    let quoted = line
        .split('"')
        .nth(1)
        .expect("DEFAULT_MODEL is no longer a plain string literal");
    assert_eq!(
        quoted,
        recalld::view::SHIM_MODEL,
        "recalld records `{}` on every model line, but the shim loads `{quoted}`",
        recalld::view::SHIM_MODEL
    );
}

#[test]
fn a_clip_is_registered_with_the_duration_it_actually_has() {
    // Measured: `write_pass` sizes its correction window from it.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "usb", "20260913T100000", 7.5);
    let meaning = meaning_for_registration();
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("register");
    assert_eq!(pass.added, 1);

    let (start, end, rate, channels): (String, String, i64, i64) = meaning
        .query_row(
            "SELECT start_utc, end_utc, sample_rate, channels FROM audio_segments",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .expect("row");
    assert_eq!(rate, 48000);
    assert_eq!(channels, 1);
    let span = (chrono::DateTime::parse_from_rfc3339(&end).expect("end")
        - chrono::DateTime::parse_from_rfc3339(&start).expect("start"))
    .num_milliseconds();
    assert!(
        (7400..=7600).contains(&span),
        "span {span}ms should be the clip's real 7.5s, not a nominal minute"
    );
}

#[test]
fn the_registered_path_is_the_ingest_copy_that_is_complete() {
    // Rows point at the ingest copy, not a partial one in `<root>/<source>/`.
    let dir = tempfile::tempdir().expect("tmp");
    let name = ingest_blob(dir.path(), "usb", "20260913T100000", 1.0);
    let meaning = meaning_for_registration();
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("register");

    let path: String = meaning
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .expect("row");
    assert!(path.ends_with(&format!("ingest/usb/{name}")), "got {path}");
    assert!(std::path::Path::new(&path).is_file(), "and it must exist");
}

#[test]
fn a_clip_already_registered_is_skipped_before_any_statement_runs() {
    // The first defence: a registered basename never reaches the INSERT. The
    // second is in a_row_whose_filename_changed_is_still_not_repointed.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "usb", "20260913T100000", 1.0);
    let meaning = meaning_for_registration();
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc,
                                         sample_rate, channels)
             VALUES ('usb', '/data/usb/usb-20260913T100000.flac',
                     '2026-09-13T10:00:00+00:00', '2026-09-13T10:01:00+00:00', 48000, 1)",
            [],
        )
        .expect("existing");
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("register");
    assert_eq!(pass.added, 0);
    let path: String = meaning
        .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
        .expect("row");
    assert_eq!(path, "/data/usb/usb-20260913T100000.flac");
}

#[test]
fn a_clip_ffmpeg_cannot_read_is_retired_not_retried_for_ever() {
    // A header-only clip is ledgered, so it is not decoded every pass.
    let dir = tempfile::tempdir().expect("tmp");
    let source_dir = recalld::store::source_dir(dir.path(), "usb");
    std::fs::create_dir_all(&source_dir).expect("dir");
    std::fs::write(source_dir.join("usb-20260913T100000.flac"), b"fLaC").expect("tombstone");
    let ingest = recalld::store::open(dir.path()).expect("ingest");
    ingest
        .execute(
            "INSERT INTO segments (filename, source, start_utc, bytes, sha256, received_utc)
             VALUES ('usb-20260913T100000.flac', 'usb', '2026-09-13T10:00:00Z', 4, 'x',
                     '2026-09-13T10:00:00Z')",
            [],
        )
        .expect("segment");
    let meaning = meaning_for_registration();

    let first =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("first");
    assert_eq!(first.unreadable, 1);
    assert_eq!(first.added, 0);

    let second =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("second");
    assert_eq!(
        second,
        recalld::registrar::Registered::default(),
        "a tombstone must be decided once, not decoded on every pass"
    );
    let outcome: String = ingest
        .query_row(
            "SELECT outcome FROM pass_ledger WHERE kind = ?1",
            [PassKind::Register],
            |r| r.get(0),
        )
        .expect("ledger");
    assert_eq!(outcome, "unreadable");
}

#[test]
fn a_source_the_meaning_plane_cannot_type_waits_rather_than_being_guessed() {
    // A source's kind is the sender's to declare: an unknown one waits.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "newmic", "20260913T100000", 1.0);
    let meaning = meaning_for_registration(); // knows `usb` only
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("register");
    assert_eq!(pass.waiting, 1);
    assert_eq!(pass.added, 0);
    let rows: i64 = meaning
        .query_row("SELECT count(*) FROM sources", [], |r| r.get(0))
        .expect("sources");
    assert_eq!(rows, 1, "and no source was invented");
}

#[test]
fn a_row_whose_filename_changed_is_still_not_repointed() {
    // The second defence: when a stored path's filename differs, only
    // `OR IGNORE` stops the INSERT repointing a row under playable turns.
    let dir = tempfile::tempdir().expect("tmp");
    ingest_blob(dir.path(), "usb", "20260913T100000", 1.0);
    let meaning = meaning_for_registration();
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc,
                                         sample_rate, channels)
             VALUES ('usb', '/data/usb/renamed-by-something-else.flac',
                     '2026-09-13T10:00:00+00:00', '2026-09-13T10:01:00+00:00', 48000, 1)",
            [],
        )
        .expect("existing");
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    let pass =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("register");
    assert_eq!(pass.added, 0, "the unique constraint must absorb it");
    let (rows, path): (i64, String) = meaning
        .query_row("SELECT count(*), max(path) FROM audio_segments", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("rows");
    assert_eq!(
        rows, 1,
        "and must not mint a second row for the same minute"
    );
    assert_eq!(
        path, "/data/usb/renamed-by-something-else.flac",
        "the path a turn can already play must stand"
    );
}

/// Two files share a minute: the second insert is ignored, and its ledger row
/// keeps it from being decoded every pass.
#[test]
fn a_clip_whose_minute_a_sibling_already_holds_is_retired_not_reconsidered() {
    let dir = tempfile::tempdir().expect("tmp");
    let meaning = meaning_for_registration();
    let ingest = recalld::store::open(dir.path()).expect("ingest");

    // Two files, one minute, as the phones produce.
    ingest_blob(dir.path(), "usb", "20260913T100000", 3.0);
    let twin = twin_blob(dir.path(), "usb", "20260913T100000", 3.0, "wav");

    let first =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("first");
    assert_eq!(first.added, 1, "one of the two takes the row");
    assert_eq!(first.covered, 1, "the other is covered by its sibling");
    // The sibling's start time is in its name and the minute is registered,
    // so it is decided without a decode.
    assert_eq!(
        first.probed, 1,
        "only the clip that took the row may be decoded; the sibling costs nothing"
    );

    // Both are ledgered, so neither is a candidate again.
    let ledgered: i64 = ingest
        .query_row(
            "SELECT count(*) FROM pass_ledger WHERE kind = ?1",
            [PassKind::Register],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(ledgered, 2, "every terminal decision writes a ledger row");
    let outcome: String = ingest
        .query_row(
            "SELECT outcome FROM pass_ledger WHERE kind = ?1 AND filename = ?2",
            (PassKind::Register, &twin),
            |r| r.get(0),
        )
        .expect("the twin's row");
    assert_eq!(
        outcome, "covered-by-sibling",
        "and it says WHY, not just that"
    );

    // The second pass does nothing: no probe, no insert.
    let second =
        register_segments(&meaning, &ingest, dir.path(), &crate::stamp(NOW), 10).expect("second");
    assert_eq!(
        (second.added, second.covered, second.retired, second.probed),
        (0, 0, 0, 0),
        "a decided clip is never looked at again, and nothing is decoded"
    );
}
