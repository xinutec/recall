//! A restored archive is checked against its own records: every stored file
//! present with the size and hash its row recorded, every meaning-plane audio
//! row naming a file that exists.

use audiocore::instant::IngestStamp;
use recalld::store::Row;
use recalld::verify::{self, Verified};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

struct Archive {
    _dir: tempfile::TempDir,
    root: PathBuf,
    meaning: Connection,
    ingest: Connection,
}

fn archive() -> Archive {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    let meaning = recalld::work::open_write(&root).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let ingest = recalld::store::open(&root).expect("ingest");
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind) VALUES ('geb', 'geb', 'tcp_pcm')",
            [],
        )
        .expect("source");
    Archive {
        _dir: dir,
        root,
        meaning,
        ingest,
    }
}

impl Archive {
    /// A stored file with its row, recording `recorded` as its bytes.
    fn store(&self, filename: &str, bytes: &[u8], recorded: &[u8]) -> PathBuf {
        let dir = recalld::store::source_dir(&self.root, "geb");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join(filename);
        std::fs::write(&path, bytes).expect("file");
        recalld::store::insert(
            &self.ingest,
            &Row {
                source: "geb".to_owned(),
                filename: filename.to_owned(),
                start_utc: IngestStamp::parse("2026-10-01T12:00:00Z").expect("instant"),
                bytes: u64::try_from(recorded.len()).expect("small"),
                sha256: hex::encode(Sha256::digest(recorded)),
                received_utc: IngestStamp::parse("2026-10-01T12:01:00Z").expect("instant"),
                sent_utc: None,
            },
        )
        .expect("row");
        path
    }

    fn audio_row(&self, path: &std::path::Path) {
        self.meaning
            .execute(
                "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
                 VALUES ('geb', ?1, '2026-10-01T12:00:00+00:00', '2026-10-01T12:01:00+00:00', 48000, 1)",
                [path.to_string_lossy()],
            )
            .expect("audio row");
    }

    fn verify(&self) -> Verified {
        verify::run(&self.meaning, &self.ingest, &self.root).expect("verify")
    }
}

#[test]
fn an_archive_whose_files_match_their_rows_has_no_faults() {
    let a = archive();
    let path = a.store("geb-20261001T120000.flac", b"minute one", b"minute one");
    a.audio_row(&path);
    a.store("geb-20261001T120100.flac", b"minute two", b"minute two");
    let v = a.verify();
    assert_eq!(v.files, 2);
    assert_eq!(v.bytes, 20);
    assert_eq!(v.audio_rows, 1);
    assert_eq!(v.faults, 0);
    assert!(v.examples.is_empty());
}

#[test]
fn a_missing_file_a_short_file_and_a_changed_file_are_each_a_fault() {
    let a = archive();
    let gone = a.store("geb-20261001T120000.flac", b"minute one", b"minute one");
    std::fs::remove_file(&gone).expect("rm");
    a.store("geb-20261001T120100.flac", b"minute", b"minute two");
    a.store(
        "geb-20261001T120200.flac",
        b"minute 3!!",
        b"minute three"[..10].as_ref(),
    );
    let v = a.verify();
    assert_eq!(v.files, 3);
    assert_eq!((v.missing, v.wrong_size, v.wrong_hash), (1, 1, 1));
    assert_eq!(v.faults, 3);
    assert_eq!(v.examples.len(), 3);
    assert!(
        v.examples
            .iter()
            .any(|e| e.contains("geb-20261001T120000.flac"))
    );
}

#[test]
fn an_audio_row_naming_no_file_is_a_fault() {
    let a = archive();
    a.audio_row(&a.root.join("ingest/geb/geb-20261001T120000.flac"));
    let v = a.verify();
    assert_eq!((v.audio_rows, v.audio_rows_missing), (1, 1));
    assert_eq!(v.faults, 1);
}

#[test]
fn examples_stop_at_the_cap_while_the_counts_go_on() {
    let a = archive();
    for i in 0..(verify::EXAMPLES + 5) {
        let p = a.store(&format!("geb-20261001T12{i:02}00.flac"), b"x", b"x");
        std::fs::remove_file(p).expect("rm");
    }
    let v = a.verify();
    assert_eq!(v.missing, verify::EXAMPLES + 5);
    assert_eq!(v.examples.len(), verify::EXAMPLES);
}
