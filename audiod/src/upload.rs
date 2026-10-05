//! Store-and-forward delivery: PUT each closed segment to recalld, check the
//! sha-256 receipt against the local bytes, and record what is proven
//! delivered. It never evicts: the Mac's archive stays the master. Its own
//! process, off the segment ffmpeg has open.

use audiocore::names;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a source's newest segment must sit unmodified to count as closed:
/// ffmpeg touches the open one on every write.
pub const OPEN_GRACE: Duration = Duration::from_mins(3);

pub struct Config {
    /// The archive root — the same `--root` capture and ingest use.
    pub root: PathBuf,
    /// recalld's base URL, e.g. `https://recall.xinutec.org`.
    pub base_url: String,
    /// The ingest bearer token; `None` sends no header (an open dev server).
    pub token: Option<String>,
    /// Per pass, so a backfill goes in resumable bites.
    pub max_per_pass: usize,
    pub open_grace: Duration,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PassSummary {
    pub uploaded: usize,
    pub failed: usize,
    pub conflicted: usize,
}

// --- delivery state ------------------------------------------------------------------

/// The uploader's bookkeeping. `uploads`: verified receipts. `conflicts`:
/// 409s, a name taken by different bytes, for a person to look at.
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

/// Everything shippable now, oldest first, less each source's newest file
/// while ffmpeg may still be writing it.
fn scan(root: &Path, grace: Duration) -> std::io::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let source = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir() || !names::valid_source(&source) {
            continue;
        }
        let mut names: Vec<String> = std::fs::read_dir(entry.path())?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| names::parse(&source, name).is_ok_and(|n| n.ext.recorded()))
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
    // A 2xx alone proves nothing: the receipt must match our own hash.
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

/// One bounded pass: scan, deliver, record. A failure waits for the next pass;
/// the files are the state.
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
                        // rusqlite refuses a u64; a file size fits an i64.
                        i64::try_from(bytes).expect("a byte count fits SQLite's i64"),
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
                // Taken by different bytes: journal it for a person, stop
                // resending.
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
