//! An upload stored before the ingest plane existed moves into it, and is then
//! leased like any other: the file, its row, and both planes' paths follow.

use audiocore::job::Kind;
use chrono::{TimeZone, Utc};
use recalld::old_uploads::{self, Moved};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

const SOURCE: &str = "meeting-20260520-1901";
const FILE: &str = "meeting-20260520-1901-20260520T180121.mp3";
const AUDIO: &[u8] = b"the meeting's bytes";

struct Fleet {
    _dir: tempfile::TempDir,
    root: PathBuf,
    meaning: Connection,
    ingest: Connection,
}

impl Fleet {
    fn old_path(&self) -> PathBuf {
        self.root.join(SOURCE).join(FILE)
    }

    fn new_path(&self) -> PathBuf {
        recalld::store::source_dir(&self.root, SOURCE).join(FILE)
    }

    fn run(&self, apply: bool) -> Moved {
        old_uploads::run(&self.root, &self.meaning, &self.ingest, now(), apply).expect("run")
    }

    fn audio_path(&self) -> String {
        self.meaning
            .query_row("SELECT path FROM audio_segments", [], |r| r.get(0))
            .expect("audio row")
    }

    fn clip(&self) -> (i64, String) {
        self.ingest
            .query_row(
                "SELECT id, path FROM clips WHERE filename = ?1",
                [FILE],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("clip")
    }

    fn leased(&self) -> Option<recalld::queue::Job> {
        recalld::queue::lease(&self.root, now(), &[Kind::TranscribeSegment]).expect("lease")
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 11, 8, 0, 0).unwrap()
}

/// As the fleet holds the oldest uploads: the file in `<root>/<source>/`, its
/// audio row and clip, and a transcription queued for it.
fn fleet() -> Fleet {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path().to_path_buf();
    let meaning = recalld::work::open_write(&root).expect("meaning");
    recalld::meaning_schema::ensure(&meaning).expect("schema");
    let ingest = recalld::store::open(&root).expect("ingest");
    std::fs::create_dir_all(root.join(SOURCE)).expect("dir");
    std::fs::write(root.join(SOURCE).join(FILE), AUDIO).expect("file");
    meaning
        .execute(
            "INSERT INTO sources (id, name, kind) VALUES (?1, 'a meeting', 'upload')",
            [SOURCE],
        )
        .expect("source");
    meaning
        .execute(
            "INSERT INTO audio_segments (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, '2026-05-20T18:01:21+00:00', '2026-05-20T18:47:19+00:00', 44100, 2)",
            (SOURCE, root.join(SOURCE).join(FILE).to_string_lossy()),
        )
        .expect("audio");
    recalld::clips::adopt_outside_ingest(&meaning, &ingest, &root).expect("clip");
    ingest
        .execute(
            "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, '2026-10-05T08:41:17Z')",
            (Kind::TranscribeSegment, FILE),
        )
        .expect("job");
    Fleet {
        _dir: dir,
        root,
        meaning,
        ingest,
    }
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read")
}

#[test]
fn an_old_upload_moves_into_the_ingest_plane_and_is_leased() {
    let r = fleet();
    assert!(
        r.leased().is_none(),
        "before the move the queue cannot see it"
    );
    let (clip, _) = r.clip();

    assert_eq!(r.run(true).moved, vec![FILE.to_owned()]);

    assert!(!r.old_path().exists());
    assert!(!r.root.join(SOURCE).exists(), "the emptied directory goes");
    assert_eq!(read(&r.new_path()), AUDIO);
    assert_eq!(r.audio_path(), r.new_path().to_string_lossy());
    assert_eq!(r.clip(), (clip, format!("ingest/{SOURCE}/{FILE}")));
    let row = recalld::store::lookup(&r.ingest, FILE)
        .expect("lookup")
        .expect("a segments row");
    assert_eq!(row.source, SOURCE);
    assert_eq!(row.start_utc, "2026-05-20T18:01:21Z");
    assert_eq!(row.bytes, AUDIO.len() as u64);
    let job = r.leased().expect("the queue offers it");
    assert_eq!((job.filename.as_str(), job.source.as_str()), (FILE, SOURCE));

    assert_eq!(r.run(true), Moved::default(), "a second run finds nothing");
}

#[test]
fn a_dry_run_moves_nothing() {
    let r = fleet();

    assert_eq!(r.run(false).moved, vec![FILE.to_owned()]);

    assert_eq!(read(&r.old_path()), AUDIO);
    assert!(!r.new_path().exists());
    assert_eq!(r.audio_path(), r.old_path().to_string_lossy());
    assert!(r.leased().is_none());
}

#[test]
fn a_run_cut_short_after_the_link_is_finished_by_the_next() {
    let r = fleet();
    std::fs::create_dir_all(r.new_path().parent().expect("dir")).expect("dir");
    std::fs::hard_link(r.old_path(), r.new_path()).expect("link");

    assert_eq!(r.run(true).moved, vec![FILE.to_owned()]);

    assert!(!r.old_path().exists());
    assert_eq!(r.audio_path(), r.new_path().to_string_lossy());
    assert!(r.leased().is_some());
}

#[test]
fn an_upload_whose_file_is_gone_is_skipped_and_left_as_it_is() {
    let r = fleet();
    std::fs::remove_file(r.old_path()).expect("rm");

    let done = r.run(true);

    assert!(done.moved.is_empty());
    assert_eq!(done.skipped.len(), 1);
    assert_eq!(r.audio_path(), r.old_path().to_string_lossy());
}
