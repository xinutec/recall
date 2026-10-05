//! Bound the launchd agents' log files.
//!
//! launchd holds the agents' logs open with `O_APPEND`, so they cannot be
//! renamed. Copytruncate instead: keep the tail in a `.1` sibling and truncate
//! the original; the writer carries on at offset 0. Lines written during the
//! copy are lost.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Rotate above this, and keep this much, so one log costs at most twice this.
/// 2 MB is roughly 20,000 lines: days of an agent's output.
pub const CAP_BYTES: u64 = 2 * 1024 * 1024;

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    pub examined: usize,
    pub rotated: usize,
    pub freed_bytes: u64,
}

/// Rotate every `*.log` in `dir` that exceeds `cap`.
///
/// # Errors
/// If the directory cannot be read; an unreadable file is skipped.
pub fn run(dir: &Path, cap: u64) -> std::io::Result<Pass> {
    let mut pass = Pass::default();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "log") {
            continue;
        }
        pass.examined += 1;
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() <= cap {
            continue;
        }
        if rotate(&path, cap).is_ok() {
            pass.rotated += 1;
            pass.freed_bytes += meta.len().saturating_sub(cap);
        }
    }
    Ok(pass)
}

/// Keep the last `cap` bytes in `<name>.1`, then truncate the original.
fn rotate(path: &Path, cap: u64) -> std::io::Result<()> {
    let mut file = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(cap)))?;
    let mut tail = Vec::with_capacity(cap as usize);
    file.read_to_end(&mut tail)?;
    // Start at the first line break, so the kept tail begins with a whole line.
    let from = tail.iter().position(|b| *b == b'\n').map_or(0, |i| i + 1);
    let mut kept = fs::File::create(path.with_extension("log.1"))?;
    kept.write_all(&tail[from..])?;
    kept.sync_all()?;
    // `set_len`, not a recreate: the agent holds this inode open.
    file.set_len(0)?;
    Ok(())
}
