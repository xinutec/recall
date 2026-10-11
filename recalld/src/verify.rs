//! Whether an archive's files agree with its records: every stored file
//! present, with the size and sha256 its `segments` row recorded, and every
//! meaning-plane audio row naming a file that exists.
//!
//! Run by hand or by the restore drill (`recalld --root <root> verify`); reads
//! every stored byte, and writes nothing beyond what opening the planes does.

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

crate::statements! {
    SEGMENTS: Ingest = "SELECT source, filename, bytes, sha256 FROM segments ORDER BY source, filename";
    AUDIO_PATHS: Meaning = "SELECT path FROM audio_segments ORDER BY path";
}

/// How many faults are named in [`Verified::examples`]; the counts go on.
pub const EXAMPLES: usize = 20;

/// What a verification found.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Verified {
    /// Stored files the ingest plane records.
    pub files: usize,
    /// Their recorded bytes.
    pub bytes: u64,
    pub missing: usize,
    pub wrong_size: usize,
    pub wrong_hash: usize,
    pub audio_rows: usize,
    pub audio_rows_missing: usize,
    /// Every fault above, summed: zero is an archive that agrees with itself.
    pub faults: usize,
    /// The first faults, each naming its file.
    pub examples: Vec<String>,
}

impl Verified {
    fn fault(&mut self, what: String) {
        self.faults += 1;
        if self.examples.len() < EXAMPLES {
            self.examples.push(what);
        }
    }
}

/// The sha256 of a file, read in pieces: an uploaded meeting can be large.
fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Check every stored file and audio row under `root`.
///
/// # Errors
/// If a plane cannot be read. A file that cannot be read is a fault, not an
/// error.
pub fn run(meaning: &Connection, ingest: &Connection, root: &Path) -> rusqlite::Result<Verified> {
    let mut v = Verified::default();
    let rows: Vec<(String, String, i64, String)> = SEGMENTS
        .prepare(ingest)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (source, filename, recorded, sha256) in rows {
        v.files += 1;
        let recorded = u64::try_from(recorded).unwrap_or(u64::MAX);
        v.bytes = v.bytes.saturating_add(recorded);
        let path = crate::store::source_dir(root, &source).join(&filename);
        match std::fs::metadata(&path) {
            Err(_) => {
                v.missing += 1;
                v.fault(format!("missing: {source}/{filename}"));
            }
            Ok(meta) if meta.len() != recorded => {
                v.wrong_size += 1;
                v.fault(format!(
                    "size: {source}/{filename} is {} bytes, recorded {recorded}",
                    meta.len()
                ));
            }
            Ok(_) => match sha256_of(&path) {
                Ok(found) if found == sha256 => {}
                Ok(_) => {
                    v.wrong_hash += 1;
                    v.fault(format!("sha256: {source}/{filename}"));
                }
                Err(err) => {
                    v.missing += 1;
                    v.fault(format!("unreadable: {source}/{filename}: {err}"));
                }
            },
        }
    }
    let paths: Vec<String> = AUDIO_PATHS
        .prepare(meaning)?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for path in paths {
        v.audio_rows += 1;
        if !Path::new(&path).exists() {
            v.audio_rows_missing += 1;
            v.fault(format!("audio row names no file: {path}"));
        }
    }
    Ok(v)
}
