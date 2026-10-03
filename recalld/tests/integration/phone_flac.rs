//! The phones' WAV copies become FLAC with nothing lost: the samples, every row
//! that names the clip, and the clip's place in the meaning plane.

use recalld::phone_flac::{self, Converted};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const WAV: &str = "pixel5-20260920T120000.wav";
const FLAC: &str = "pixel5-20260920T120000.phone.flac";
const START: &str = "2026-09-20T12:00:00Z";
const BEFORE: &str = "2026-10-01T00:00:00Z";

fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data = u32::try_from(samples.len() * 2).expect("small");
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&96_000u32.to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    b
}

fn speech() -> Vec<i16> {
    (0..48_000 * 2)
        .map(|i: i32| {
            let t = f64::from(i) / 48_000.0;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "within i16 by construction"
            )]
            let s = (6000.0 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()) as i16;
            s.wrapping_add(i16::try_from(i * 7919 % 61).expect("small") - 30)
        })
        .collect()
}

struct Fleet {
    _dir: tempfile::TempDir,
    root: PathBuf,
    meaning: Connection,
    ingest: Connection,
}

impl Fleet {
    fn clips(&self) -> PathBuf {
        recalld::store::source_dir(&self.root, "pixel5")
    }
}

/// One phone clip as the fleet holds it: the file, its rows in both planes.
fn root_with(source_kind: &str, wav: &[u8], job_state: &str) -> Fleet {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    let meaning = recalld::work::open_write(&root).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let ingest = recalld::store::open(&root).expect("ingest");
    let clips = recalld::store::source_dir(&root, "pixel5");
    std::fs::create_dir_all(&clips).expect("dir");
    std::fs::write(clips.join(WAV), wav).expect("wav");
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind) VALUES ('pixel5', 'pixel5', ?1)",
            [source_kind],
        )
        .expect("source");
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES ('pixel5', ?1, '2026-09-20T12:00:00+00:00', '2026-09-20T12:01:00+00:00', 48000, 1)",
            [clips.join(WAV).to_string_lossy()],
        )
        .expect("clip");
    ingest
        .execute_batch(&format!(
            "INSERT INTO segments (filename, source, start_utc, bytes, sha256, received_utc)
                 VALUES ('{WAV}', 'pixel5', '{START}', 1, 'old', '2026-09-20T12:02:00Z');
             INSERT INTO segment_levels (filename, source, speech_db, floor_db, computed_utc)
                 VALUES ('{WAV}', 'pixel5', -30, -60, '{START}');
             INSERT INTO segment_speech (filename, source, speech_seconds, computed_utc)
                 VALUES ('{WAV}', 'pixel5', 12.5, '{START}');
             INSERT INTO jobs (kind, filename, state, created_utc)
                 VALUES ('transcribe-segment', '{WAV}', '{job_state}', '{START}');
             INSERT INTO pass_ledger (kind, filename, outcome, decided_utc)
                 VALUES ('register-segment', '{WAV}', 'registered', '{START}');"
        ))
        .expect("rows");
    Fleet {
        _dir: dir,
        root,
        meaning,
        ingest,
    }
}

fn run(r: &mut Fleet, apply: bool) -> Converted {
    phone_flac::convert(&r.root, &r.meaning, &mut r.ingest, BEFORE, apply).expect("convert")
}

/// Decoded by ffmpeg, independently of the code under test.
fn decoded(path: &Path) -> Vec<i16> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "s16le", "-"])
        .output()
        .expect("ffmpeg");
    assert!(out.status.success());
    out.stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect()
}

fn names(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT filename FROM {table}"))
        .expect("prepare");
    stmt.query_map([], |r| r.get(0))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

#[test]
fn a_phones_wav_becomes_flac_with_the_same_samples_and_every_row_renamed() {
    let samples = speech();
    let mut r = root_with("tcp_pcm", &wav_bytes(&samples), "done");
    let done = run(&mut r, true);

    assert_eq!(done.converted, 1, "{done:?}");
    assert!(done.flac_bytes * 2 < done.wav_bytes, "{done:?}");
    assert!(!r.clips().join(WAV).exists());
    assert_eq!(decoded(&r.clips().join(FLAC)), samples);
    for table in [
        "segments",
        "segment_levels",
        "segment_speech",
        "jobs",
        "pass_ledger",
    ] {
        assert_eq!(names(&r.ingest, table), [FLAC], "{table}");
    }
    let (bytes, sha): (i64, String) = r
        .ingest
        .query_row("SELECT bytes, sha256 FROM segments", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("row");
    let file = std::fs::read(r.clips().join(FLAC)).expect("flac");
    assert_eq!(usize::try_from(bytes).expect("size"), file.len());
    assert_ne!(sha, "old");
    let path: String = r
        .meaning
        .query_row("SELECT path FROM audio_segments", [], |row| row.get(0))
        .expect("path");
    assert_eq!(path, r.clips().join(FLAC).to_string_lossy());
}

#[test]
fn a_wav_cut_short_by_a_crash_keeps_every_sample() {
    // The phone patches the header at close; a crash leaves it saying zero.
    let samples = speech();
    let mut wav = wav_bytes(&samples);
    wav[4..8].copy_from_slice(&36u32.to_le_bytes());
    wav[40..44].copy_from_slice(&0u32.to_le_bytes());
    let mut r = root_with("tcp_pcm", &wav, "done");

    assert_eq!(run(&mut r, true).converted, 1);
    assert_eq!(decoded(&r.clips().join(FLAC)), samples);
}

#[test]
fn a_dry_run_changes_nothing() {
    let wav = wav_bytes(&speech());
    let mut r = root_with("tcp_pcm", &wav, "done");
    let done = run(&mut r, false);

    assert_eq!(done.converted, 1);
    assert_eq!(std::fs::read(r.clips().join(WAV)).expect("wav"), wav);
    assert!(!r.clips().join(FLAC).exists());
    assert_eq!(names(&r.ingest, "segments"), [WAV]);
}

#[test]
fn a_clip_a_job_is_working_on_is_left_alone() {
    let mut r = root_with("tcp_pcm", &wav_bytes(&speech()), "leased");

    assert_eq!(run(&mut r, true).converted, 0);
    assert!(r.clips().join(WAV).exists());
}

#[test]
fn an_uploaded_recording_stays_as_it_was_given() {
    let mut r = root_with("upload", &wav_bytes(&speech()), "done");

    assert_eq!(run(&mut r, true), Converted::default());
    assert!(r.clips().join(WAV).exists());
}

#[test]
fn a_wav_of_another_shape_is_left_for_a_person() {
    let mut wav = wav_bytes(&speech());
    wav[22] = 2; // stereo
    let mut r = root_with("tcp_pcm", &wav, "done");
    let done = run(&mut r, true);

    assert_eq!(done.converted, 0);
    assert_eq!(done.skipped.len(), 1);
    assert!(r.clips().join(WAV).exists());
}

#[test]
fn a_run_cut_off_after_the_flac_landed_is_finished_by_the_next() {
    let samples = speech();
    let mut r = root_with("tcp_pcm", &wav_bytes(&samples), "done");
    // Where an earlier run stopped: the FLAC written, nothing renamed yet.
    let landed = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(r.clips().join(WAV))
        .arg(r.clips().join(FLAC))
        .status()
        .expect("ffmpeg");
    assert!(landed.success());

    assert_eq!(run(&mut r, true).converted, 1);
    assert!(!r.clips().join(WAV).exists());
    assert_eq!(names(&r.ingest, "segments"), [FLAC]);
    assert_eq!(decoded(&r.clips().join(FLAC)), samples);
}
