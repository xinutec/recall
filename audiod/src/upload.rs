//! Store-and-forward delivery (docs/architecture.md, stage B): walk the
//! archive for closed segments, PUT each to recalld's ingest plane, verify
//! the sha-256 receipt against the local bytes, and record what is proven
//! delivered. The recorder contract's first half — eviction (the second half)
//! is deliberately NOT here: the Mac's archive stays the protected master
//! until stage F, so this module only ever adds copies.
//!
//! Never in the capture path: this runs as its own process over files ffmpeg
//! has finished with. The one point of contact is the scan rule below that
//! keeps it off the segment ffmpeg still has open.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the lexically-newest segment of a source must sit unmodified
/// before it is believed closed. ffmpeg touches the open segment on every
/// write, so "newest and recently modified" is the live file; a newest file
/// this stale means capture stopped and the ring's last segment is final.
pub const OPEN_GRACE: Duration = Duration::from_mins(3);

pub struct Config {
    /// The archive root — the same `--root` capture and ingest use.
    pub root: PathBuf,
    /// recalld's base URL, e.g. `http://10.100.0.2:8001`.
    pub base_url: String,
    /// The ingest bearer token; `None` sends no header (an open dev server).
    pub token: Option<String>,
    /// Upper bound per pass, so a historical backfill proceeds in bounded,
    /// resumable bites rather than one marathon.
    pub max_per_pass: usize,
    pub open_grace: Duration,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PassSummary {
    pub uploaded: usize,
    pub failed: usize,
    pub conflicted: usize,
}

/// The extensions a segment ring writes — must stay the subset recalld's
/// name grammar accepts (`recalld/src/names.rs`; unified in stage D's shared
/// crate).
const EXTENSIONS: [&str; 4] = ["flac", "opus", "ogg", "wav"];

/// `<source>-YYYYMMDDTHHMMSS.<ext>`, the archive naming contract. Anything
/// else in a source directory (ffmpeg temp files, sidecars) is not ours to
/// ship.
fn is_segment_of(source: &str, filename: &str) -> bool {
    let Some(rest) = filename
        .strip_prefix(source)
        .and_then(|r| r.strip_prefix('-'))
    else {
        return false;
    };
    let Some((stamp, ext)) = rest.split_once('.') else {
        return false;
    };
    EXTENSIONS.contains(&ext)
        && stamp.len() == 15
        && stamp.bytes().enumerate().all(|(i, b)| {
            if i == 8 {
                b == b'T'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn valid_source(source: &str) -> bool {
    let mut chars = source.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    source.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

// --- delivery state ------------------------------------------------------------------

/// The uploader's own bookkeeping, beside the archive it mirrors. A row in
/// `uploads` is a receipt that VERIFIED (hash equality against our own
/// re-read); a row in `conflicts` is a 409 — the name is taken by different
/// bytes, which retrying cannot fix and a person must look at.
fn open_state(root: &Path) -> rusqlite::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(root.join("upload-state.sqlite"))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS uploads (
             filename     TEXT PRIMARY KEY,
             source       TEXT NOT NULL,
             sha256       TEXT NOT NULL,
             bytes        INTEGER NOT NULL,
             verified_utc TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS conflicts (
             filename     TEXT PRIMARY KEY,
             source       TEXT NOT NULL,
             sha256       TEXT NOT NULL,
             noticed_utc  TEXT NOT NULL
         );",
    )?;
    Ok(conn)
}

fn already_handled(conn: &rusqlite::Connection, filename: &str) -> bool {
    let hit = |sql: &str| conn.query_row(sql, [filename], |_| Ok(())).is_ok();
    hit("SELECT 1 FROM uploads WHERE filename = ?1")
        || hit("SELECT 1 FROM conflicts WHERE filename = ?1")
}

// --- the pass ------------------------------------------------------------------------

struct Candidate {
    source: String,
    filename: String,
    path: PathBuf,
}

/// Everything shippable right now, oldest first. The lexically-newest file of
/// each source is skipped while its mtime is fresh — that is the segment
/// ffmpeg may still be writing.
fn scan(root: &Path, grace: Duration) -> std::io::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let source = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir() || !valid_source(&source) {
            continue;
        }
        let mut names: Vec<String> = std::fs::read_dir(entry.path())?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| is_segment_of(&source, name))
            .collect();
        names.sort();
        let newest_open = names.last().is_some_and(|newest| {
            let path = entry.path().join(newest);
            path.metadata()
                .and_then(|m| m.modified())
                .and_then(|t| t.elapsed().map_err(std::io::Error::other))
                .is_ok_and(|age| age < grace)
        });
        if newest_open {
            names.pop();
        }
        for name in names {
            out.push(Candidate {
                path: entry.path().join(&name),
                source: source.clone(),
                filename: name,
            });
        }
    }
    out.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok(out)
}

enum Delivery {
    Verified { sha256: String, bytes: usize },
    Conflict { sha256: String },
    Failed(String),
}

fn deliver(config: &Config, candidate: &Candidate, agent: &ureq::Agent) -> Delivery {
    let bytes = match std::fs::read(&candidate.path) {
        Ok(bytes) => bytes,
        Err(err) => return Delivery::Failed(format!("read: {err}")),
    };
    let sha256 = hex::encode(Sha256::digest(&bytes));
    let url = format!(
        "{}/ingest/v1/segments/{}/{}",
        config.base_url, candidate.source, candidate.filename
    );
    let mut request = agent
        .put(&url)
        .set("x-recall-sent", &now_rfc3339())
        .set("content-type", "application/octet-stream");
    if let Some(token) = &config.token {
        request = request.set("authorization", &format!("Bearer {token}"));
    }
    let response = match request.send_bytes(&bytes) {
        Ok(response) => response,
        Err(ureq::Error::Status(409, _)) => return Delivery::Conflict { sha256 },
        Err(err) => return Delivery::Failed(err.to_string()),
    };
    let body = match response.into_string() {
        Ok(body) => body,
        Err(err) => return Delivery::Failed(format!("receipt read: {err}")),
    };
    let receipt: serde_json::Value = match serde_json::from_str(&body) {
        Ok(receipt) => receipt,
        Err(err) => return Delivery::Failed(format!("receipt parse: {err}")),
    };
    // The eviction-grade check: the receipt must equal our OWN hash of what
    // we read from disk. A 2xx proves nothing by itself.
    if receipt["sha256"] == sha256.as_str() && receipt["bytes"] == bytes.len() {
        Delivery::Verified {
            sha256,
            bytes: bytes.len(),
        }
    } else {
        Delivery::Failed(format!("receipt disagrees: {body}"))
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// One bounded pass: scan, deliver, record. Individual failures are logged
/// and left for the next pass — the files are the state, so nothing needs a
/// retry queue.
pub fn run_pass(config: &Config) -> PassSummary {
    let mut summary = PassSummary::default();
    let conn = match open_state(&config.root) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!(%err, "upload state unavailable");
            summary.failed = 1;
            return summary;
        }
    };
    let candidates = match scan(&config.root, config.open_grace) {
        Ok(candidates) => candidates,
        Err(err) => {
            tracing::error!(%err, "archive scan failed");
            summary.failed = 1;
            return summary;
        }
    };
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(5))
        .timeout(Duration::from_mins(2))
        .build();
    for candidate in candidates {
        if summary.uploaded + summary.failed + summary.conflicted >= config.max_per_pass {
            break;
        }
        if already_handled(&conn, &candidate.filename) {
            continue;
        }
        match deliver(config, &candidate, &agent) {
            Delivery::Verified { sha256, bytes } => {
                let recorded = conn.execute(
                    "INSERT OR IGNORE INTO uploads
                         (filename, source, sha256, bytes, verified_utc)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    (
                        &candidate.filename,
                        &candidate.source,
                        &sha256,
                        bytes as u64,
                        now_rfc3339(),
                    ),
                );
                match recorded {
                    Ok(_) => summary.uploaded += 1,
                    Err(err) => {
                        // Delivered but not recorded: the next pass re-sends
                        // and the server's idempotent PUT absorbs it.
                        tracing::error!(%err, file = %candidate.filename, "verified but not recorded");
                        summary.failed += 1;
                    }
                }
            }
            Delivery::Conflict { sha256 } => {
                // The name is taken by different bytes. Retrying cannot fix
                // it and overwriting is forbidden by design — journal it for
                // a person and stop resending.
                tracing::error!(file = %candidate.filename, "receipt conflict: name held by different bytes");
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO conflicts (filename, source, sha256, noticed_utc)
                     VALUES (?1, ?2, ?3, ?4)",
                    (
                        &candidate.filename,
                        &candidate.source,
                        &sha256,
                        now_rfc3339(),
                    ),
                );
                summary.conflicted += 1;
            }
            Delivery::Failed(reason) => {
                tracing::warn!(file = %candidate.filename, %reason, "upload failed; will retry next pass");
                summary.failed += 1;
            }
        }
    }
    tracing::info!(
        uploaded = summary.uploaded,
        failed = summary.failed,
        conflicted = summary.conflicted,
        "upload pass complete"
    );
    summary
}
