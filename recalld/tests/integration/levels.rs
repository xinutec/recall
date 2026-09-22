//! Calibration evidence from delivered segments (stage D2): a loud segment
//! and a quiet one from the same "device" must order correctly, the scanner
//! must be idempotent, and the per-device reference must come from the rows.

use recalld::levels::{scan_once, speech_reference_db};
use recalld::store;
use std::f32::consts::PI;
use std::path::Path;

/// A mono 16 kHz WAV of a sine at `amplitude` — through audiocore's writer,
/// decoded back by the real ffmpeg path in `measure`.
fn wav_segment(path: &Path, amplitude: f32, seconds: f32) {
    let rate = 16_000u32;
    // BURSTS, not a steady tone: the real-speech reference gate admits a
    // segment only when its speech quantile clears its own floor, and speech
    // is on-off by nature — a constant sine has no floor below itself.
    let samples: Vec<f32> = (0..(seconds * rate as f32) as usize)
        .map(|i| {
            let on = (i / rate as usize).is_multiple_of(2);
            let gain = if on { amplitude } else { amplitude * 0.001 };
            gain * (2.0 * PI * 330.0 * i as f32 / rate as f32).sin()
        })
        .collect();
    audiocore::wav::write_mono16(path, rate, &samples).expect("wav");
}

fn stored_segment(root: &Path, source: &str, name: &str, amplitude: f32) {
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    wav_segment(&dir.join(name), amplitude, 2.0);
    let conn = store::open(root).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: source.to_owned(),
            filename: name.to_owned(),
            start_utc: name[name.len() - 19..name.len() - 4].to_owned(),
            bytes: 1,
            sha256: "x".to_owned(),
            received_utc: "2026-09-05T00:00:00Z".to_owned(),
            sent_utc: None,
        },
    )
    .expect("row");
}

#[test]
fn levels_order_by_how_loud_the_device_heard() {
    let dir = tempfile::tempdir().expect("tempdir");
    stored_segment(dir.path(), "usb", "usb-20260905T120000.wav", 0.5);
    stored_segment(dir.path(), "usb", "usb-20260905T120100.wav", 0.005);
    assert_eq!(scan_once(dir.path(), 100).expect("scan"), 2);
    let conn = store::open(dir.path()).expect("db");
    let loud: f64 = conn
        .query_row(
            "SELECT speech_db FROM segment_levels WHERE filename LIKE '%120000%'",
            [],
            |r| r.get(0),
        )
        .expect("loud");
    let quiet: f64 = conn
        .query_row(
            "SELECT speech_db FROM segment_levels WHERE filename LIKE '%120100%'",
            [],
            |r| r.get(0),
        )
        .expect("quiet");
    // 0.5 vs 0.005 amplitude = 40 dB apart; allow decode slop either side.
    assert!(loud > quiet + 30.0, "loud {loud} vs quiet {quiet}");
}

#[test]
fn the_scanner_measures_each_blob_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    stored_segment(dir.path(), "usb", "usb-20260905T120000.wav", 0.2);
    assert_eq!(scan_once(dir.path(), 100).expect("scan"), 1);
    assert_eq!(scan_once(dir.path(), 100).expect("rescan"), 0);
}

#[test]
fn an_undecodable_blob_is_recorded_not_retried_forever() {
    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("ingest").join("usb");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("usb-20260905T120000.wav"), b"not audio").expect("junk");
    let conn = store::open(dir.path()).expect("db");
    store::insert(
        &conn,
        &store::Row {
            source: "usb".into(),
            filename: "usb-20260905T120000.wav".into(),
            start_utc: "2026-09-05T12:00:00Z".into(),
            bytes: 1,
            sha256: "x".into(),
            received_utc: "2026-09-05T00:00:00Z".into(),
            sent_utc: None,
        },
    )
    .expect("row");
    assert_eq!(scan_once(dir.path(), 100).expect("scan"), 1);
    assert_eq!(scan_once(dir.path(), 100).expect("rescan"), 0);
}

#[test]
fn the_device_reference_is_a_query_over_its_own_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (i, amp) in [0.5f32, 0.4, 0.3, 0.05].iter().enumerate() {
        stored_segment(
            dir.path(),
            "usb",
            &format!("usb-20260905T12000{i}.wav"),
            *amp,
        );
    }
    scan_once(dir.path(), 100).expect("scan");
    // The reference is VAD-GATED (stage D4): only segments the detector says
    // carry speech feed it. These fixtures are bursts of tone, which silero
    // will not call speech, so the evidence is recorded directly — the point
    // under test is the QUANTILE over a source's own rows, not the detector.
    let conn = store::open(dir.path()).expect("db");
    recalld::ingest_schema::ensure(&conn).expect("schema");
    for i in 0..4 {
        conn.execute(
            "INSERT OR IGNORE INTO segment_speech
                 (filename, source, speech_seconds, computed_utc)
             VALUES (?1, 'usb', 30.0, '2026-09-05T12:00:00Z')",
            [format!("usb-20260905T12000{i}.wav")],
        )
        .expect("speech row");
    }
    let faintest = speech_reference_db(&conn, "usb", 0.0, 100)
        .expect("query")
        .expect("some");
    let loudest = speech_reference_db(&conn, "usb", 1.0, 100)
        .expect("query")
        .expect("some");
    assert!(faintest < loudest - 15.0, "{faintest} vs {loudest}");
    // A device with no measured rows has no reference — never a default.
    assert_eq!(
        speech_reference_db(&conn, "pixel5", 0.05, 100).expect("query"),
        None
    );
}

// ---- the gate detector (#1526) ----

use recalld::levels::{GATE_DB, gated_fraction};

#[test]
fn a_calm_room_is_not_a_gate_however_quiet_it_is() {
    // ⚠ THE FALSE POSITIVE THAT WOULD MAKE THIS UNUSABLE. A quiet evening has
    // quiet buckets scattered through it; only a gate produces CONSECUTIVE ones,
    // because it stays shut until it hears a voice. Counting quiet buckets alone
    // would condemn every calm household.
    let silent = 0.0;
    let quiet = 10f32.powf(-40.0 / 20.0); // -40 dBFS: quiet, nowhere near the gate
    let scattered = vec![
        quiet, silent, quiet, quiet, silent, quiet, silent, quiet, quiet, quiet,
    ];
    assert!(
        gated_fraction(&scattered).abs() < f32::EPSILON,
        "single silent buckets between signal are a room, not a gate"
    );
}

#[test]
fn a_run_shorter_than_the_minimum_does_not_count() {
    let s = 0.0;
    let loud = 10f32.powf(-20.0 / 20.0);
    // Two consecutive silent buckets — 0.2 s, under the 0.3 s floor.
    assert!(gated_fraction(&[loud, s, s, loud, loud]).abs() < f32::EPSILON);
    // Three — at the floor, so it counts, and it is 3 of 5 buckets.
    let three = gated_fraction(&[loud, s, s, s, loud]);
    assert!(
        (three - 0.6).abs() < 1e-6,
        "a run at the minimum must count, got {three}"
    );
}

#[test]
fn a_run_at_the_end_of_a_segment_is_not_lost() {
    // ⚠ The gate closing on the LAST word is the commonest shape, and a loop
    // that only credits a run when it SEES the run end drops exactly that one.
    let loud = 10f32.powf(-20.0 / 20.0);
    let trailing = gated_fraction(&[loud, loud, 0.0, 0.0, 0.0, 0.0]);
    assert!(
        (trailing - 4.0 / 6.0).abs() < 1e-6,
        "a trailing silent run must be counted, got {trailing}"
    );
}

#[test]
fn the_threshold_is_below_anything_a_real_front_end_produces() {
    // -80 dBFS is three LSB of a 16-bit sample. Measured 2026-09-12 over 22
    // evening segments, the USB condenser's quietest 0.1 s never reached it.
    // A bucket just ABOVE the line must not count, or a real mic's floor would.
    let just_above = 10f32.powf((GATE_DB + 2.0) / 20.0);
    assert!(gated_fraction(&[just_above; 20]).abs() < f32::EPSILON);
    let just_below = 10f32.powf((GATE_DB - 2.0) / 20.0);
    assert!((gated_fraction(&[just_below; 20]) - 1.0).abs() < f32::EPSILON);
}

#[test]
fn an_empty_envelope_is_not_gated() {
    // A segment that decoded to nothing must not read as a gated source — that
    // is a decode failure, and `speech_db` is what marks it (see `scan_once`).
    assert!(gated_fraction(&[]).abs() < f32::EPSILON);
}

#[test]
fn the_scanner_stores_the_gate_measurement() {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path();
    stored_segment(root, "usb", "usb-20260911T100000.wav", 0.2);
    assert_eq!(scan_once(root, 10).expect("scan"), 1);
    let conn = store::open(root).expect("db");
    let gated: Option<f64> = conn
        .query_row(
            "SELECT gated FROM segment_levels WHERE source = 'usb'",
            [],
            |r| r.get(0),
        )
        .expect("row");
    // A sine at -14 dBFS is never near the gate.
    let gated = gated.expect("a measured segment must store a number");
    assert!(
        gated.abs() < f64::EPSILON,
        "a sine at -14 dBFS is not gated"
    );
}

#[test]
fn a_database_from_before_the_detector_gains_the_column() {
    // ⚠ THE SHAPE THE WHOLE SUITE WAS BLIND TO. Every other test builds
    // `segment_levels` fresh, where the column arrives with the CREATE. Production
    // had the table already, and `CREATE TABLE IF NOT EXISTS` adds nothing to an
    // existing one — so `gated` was never going to appear there, and the insert
    // names it. Found by querying the live fleet: `no such column: gated`.
    let dir = tempfile::tempdir().expect("tmp");
    // A raw connection: `store::open` would create the table in its current shape.
    let conn = rusqlite::Connection::open(dir.path().join("ingest.sqlite")).expect("db");
    // The table as it stood before the detector.
    conn.execute_batch(
        "CREATE TABLE segment_levels (
             filename     TEXT PRIMARY KEY,
             source       TEXT NOT NULL,
             speech_db    REAL NOT NULL,
             floor_db     REAL NOT NULL,
             computed_utc TEXT NOT NULL
         );",
    )
    .expect("old schema");
    conn.execute(
        "INSERT INTO segment_levels VALUES ('usb-old.wav','usb',-20.0,-60.0,'2026-09-01T00:00:00Z')",
        [],
    )
    .expect("a row from before");

    recalld::ingest_schema::ensure(&conn).expect("migrate");

    let has: bool = conn
        .prepare("SELECT 1 FROM pragma_table_info('segment_levels') WHERE name = 'gated'")
        .expect("prep")
        .exists([])
        .expect("query");
    assert!(
        has,
        "an existing table must GAIN the column, not silently skip it"
    );

    // The pre-existing row keeps its readings and reads NULL for the new one —
    // unknown, which is what makes the room builder defer rather than trust it.
    let (speech, gated): (f64, Option<f64>) = conn
        .query_row(
            "SELECT speech_db, gated FROM segment_levels WHERE filename = 'usb-old.wav'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row survives");
    assert!(
        (speech - -20.0).abs() < f64::EPSILON,
        "the old reading stands"
    );
    assert!(gated.is_none(), "not measured is not zero");

    // And running it twice must not fail on the column already being there.
    recalld::ingest_schema::ensure(&conn).expect("idempotent");
}

use recalld::levels::quiet_run_seconds;

/// Decode a stored fixture exactly as `measure` does.
fn fixture_pcm(name: &str) -> Option<Vec<u8>> {
    let path = std::path::PathBuf::from("../tests/fixtures/speech").join(name);
    if !path.exists() {
        // Not deliberate when it happens: .gitignore's blanket *.flac has
        // swallowed these before (#1433).
        eprintln!("skipping: fixture {name} absent");
        return None;
    }
    audiocore::decode::decode_s16(&path, 16_000)
}

#[test]
fn a_real_recording_of_speech_holds_no_long_quiet_run() {
    // ⚠ THE DIRECTION THAT MATTERS. A gate detector earns its place by leaving
    // healthy microphones alone: the one that shipped before this flagged every
    // phone in the house, so the filter built on it had to be reverted (#1526).
    //
    // A human reading, recorded rather than synthesised, measures 0.128 s here.
    let Some(pcm) = fixture_pcm("public-domain-en.flac") else {
        return;
    };
    let run = quiet_run_seconds(&pcm, 16_000);
    assert!(
        run < 0.5,
        "a real recording must not read as gated; got {run:.3}s"
    );
}

#[test]
fn synthesised_speech_carries_digital_silence_that_a_microphone_never_would() {
    // ⚠ Recorded so it is not mistaken for evidence about real audio. Both
    // `dialogue-*` fixtures are MACHINE-READ (tests/fixtures/speech/README.md),
    // and a synthesiser emits true zeros between utterances: 18-22% of their
    // samples are exactly zero and the longest quiet stretch is ~0.81 s, against
    // 0.128 s for the human reading beside them. That is the synthesiser, not a
    // gate — so these two must never be used to calibrate this detector.
    let Some(pcm) = fixture_pcm("dialogue-en.flac") else {
        return;
    };
    let run = quiet_run_seconds(&pcm, 16_000);
    assert!(
        run > 0.5,
        "expected the synthesiser's digital silence; got {run:.3}s"
    );
}

#[test]
fn the_longest_stretch_is_what_counts_not_the_total_quiet() {
    // The whole design in one assertion. A microphone listening to a room emits
    // many short near-silences; a gate holds ONE long one. Summing them would
    // measure how quiet the room was and flag every calm conversation — which is
    // exactly how the previous detector failed.
    let rate = 16_000u32;
    let mut scattered: Vec<i16> = Vec::new();
    for _ in 0..20 {
        scattered.extend(std::iter::repeat_n(0i16, rate as usize / 10)); // 0.1 s
        scattered.extend(std::iter::repeat_n(4000i16, rate as usize / 10));
    }
    let mut gated = vec![4000i16; rate as usize];
    gated.extend(std::iter::repeat_n(0i16, 2 * rate as usize)); // one 2 s stretch
    gated.extend(std::iter::repeat_n(4000i16, rate as usize));

    let bytes = |v: &[i16]| -> Vec<u8> { v.iter().flat_map(|s| s.to_le_bytes()).collect() };
    let scattered_run = quiet_run_seconds(&bytes(&scattered), rate);
    let gated_run = quiet_run_seconds(&bytes(&gated), rate);

    assert!(
        (scattered_run - 0.1).abs() < 0.01,
        "2.0 s of quiet in twenty pieces is 0.1 s, not 2.0; got {scattered_run:.3}s"
    );
    assert!(
        (gated_run - 2.0).abs() < 0.01,
        "one 2 s stretch must measure 2 s; got {gated_run:.3}s"
    );
}

#[test]
fn a_database_that_already_has_gated_gains_the_run_column() {
    // ⚠ THE SHAPE PRODUCTION IS IN TODAY, which the pre-2026-09-12 test above
    // does NOT cover: the fleet's table already has `gated`, so a migration
    // keyed on that column alone would decide it had nothing to do and the
    // insert naming `quiet_run_s` would fail on every segment.
    let dir = tempfile::tempdir().expect("tmp");
    let conn = rusqlite::Connection::open(dir.path().join("ingest.sqlite")).expect("db");
    conn.execute_batch(
        "CREATE TABLE segment_levels (
             filename     TEXT PRIMARY KEY,
             source       TEXT NOT NULL,
             speech_db    REAL NOT NULL,
             floor_db     REAL NOT NULL,
             gated        REAL,
             computed_utc TEXT NOT NULL
         );
         INSERT INTO segment_levels
           VALUES ('usb-x.wav','usb',-20.0,-60.0,0.0,'2026-09-15T00:00:00Z');",
    )
    .expect("the schema as the fleet has it");

    recalld::ingest_schema::ensure(&conn).expect("migrate");

    let run: Option<f64> = conn
        .query_row(
            "SELECT quiet_run_s FROM segment_levels WHERE filename = 'usb-x.wav'",
            [],
            |r| r.get(0),
        )
        .expect("the column must exist");
    assert!(
        run.is_none(),
        "a row measured before this statistic reads UNKNOWN, never a clean zero"
    );
}
