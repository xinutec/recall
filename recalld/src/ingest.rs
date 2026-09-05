//! The ingest plane's handlers (docs/architecture.md, stage A): recorders PUT
//! closed segments, get a sha-256 receipt, and verify it against their own
//! re-hash before they will ever evict a local copy. Everything here serves
//! that contract:
//!
//! - **Durability before acknowledgement.** Bytes stream to a temp file in the
//!   same filesystem, are fsynced, renamed into place, the directory fsynced,
//!   and the row inserted — only then does the receipt go out. A name never
//!   points at partial bytes, and a crash leaves either nothing or a blob the
//!   next identical PUT heals a row for.
//! - **Append-only.** A re-PUT of identical bytes is idempotent (same
//!   receipt); a name collision with different bytes is 409 and the stored
//!   blob is untouched. Nothing here deletes, and no route ever will — the
//!   absence of a delete endpoint is decision 2, not an omission.
//! - **Write is the only verb a device token buys.** Read (listing, blobs) is
//!   the sync token's, so a recorder that can upload still cannot read.

use crate::app::Config;
use crate::store::{self, Row};
use crate::tokens::Verdict;
use audiocore::names::{self, SegmentName};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path as FsPath;
use std::sync::Arc;

fn error(status: StatusCode, message: &str) -> Response {
    (status, axum::Json(json!({ "error": message }))).into_response()
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// A refused credential, small enough to ride an `Err` — the caller turns it
/// into a `Response` (clippy: a full `Response` is a fat `Err` variant).
struct AuthError {
    status: StatusCode,
    message: &'static str,
}

impl AuthError {
    fn into_response(self) -> Response {
        error(self.status, self.message)
    }
}

/// The write gate: inert when no tokens file is configured, else the bearer
/// must be the named source's own token.
fn write_auth(config: &Config, headers: &HeaderMap, source: &str) -> Result<(), AuthError> {
    let Some(tokens) = &config.tokens else {
        return Ok(());
    };
    let Some(bearer) = bearer(headers) else {
        return Err(AuthError {
            status: StatusCode::UNAUTHORIZED,
            message: "missing bearer token",
        });
    };
    match tokens.check(source, bearer) {
        Verdict::Allowed => Ok(()),
        Verdict::UnknownToken => Err(AuthError {
            status: StatusCode::UNAUTHORIZED,
            message: "unknown token",
        }),
        Verdict::WrongSource => Err(AuthError {
            status: StatusCode::FORBIDDEN,
            message: "token belongs to a different source",
        }),
    }
}

/// The read gate: the sync token, when configured.
fn read_auth(config: &Config, headers: &HeaderMap) -> Result<(), AuthError> {
    let Some(expected) = &config.read_token else {
        return Ok(());
    };
    let presented = bearer(headers);
    if presented.is_some_and(|b| crate::tokens::same_token(b, expected)) {
        Ok(())
    } else {
        Err(AuthError {
            status: StatusCode::UNAUTHORIZED,
            message: "read requires the sync token",
        })
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn receipt(sha256: &str, bytes: usize) -> Response {
    (
        StatusCode::OK,
        axum::Json(json!({ "sha256": sha256, "bytes": bytes })),
    )
        .into_response()
}

const DIVERGENT: &str = "a different segment already holds this name";

/// The blocking half of a PUT: hash, store, record — called off the async
/// executor. Every early return maps a defect to the recorder's own fault
/// line: 400 says fix the name, 409 says the name is taken, 500 says retry.
fn store_segment(
    config: &Config,
    name: &SegmentName,
    filename: &str,
    body: &[u8],
    sent_utc: Option<String>,
) -> Response {
    let sha256 = sha256_hex(body);
    let conn = match store::open(&config.root) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!(%err, "ingest.sqlite unavailable");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "bookkeeping unavailable");
        }
    };
    match store::lookup(&conn, filename) {
        Ok(Some(row)) if row.sha256 == sha256 => return receipt(&sha256, body.len()),
        Ok(Some(_)) => return error(StatusCode::CONFLICT, DIVERGENT),
        Ok(None) => {}
        Err(err) => {
            tracing::error!(%err, "row lookup failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "bookkeeping unavailable");
        }
    }
    let dir = config.root.join("ingest").join(&name.source);
    let dest = dir.join(filename);
    let written = write_blob(&config.root, &dir, &dest, body);
    match written {
        Ok(WriteOutcome::Written | WriteOutcome::AlreadyIdentical) => {}
        Ok(WriteOutcome::AlreadyDivergent) => return error(StatusCode::CONFLICT, DIVERGENT),
        Err(err) => {
            tracing::error!(%err, "blob write failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "blob write failed");
        }
    }
    let row = Row {
        source: name.source.clone(),
        filename: filename.to_owned(),
        start_utc: name.start_utc.clone(),
        bytes: body.len() as u64,
        sha256: sha256.clone(),
        received_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sent_utc,
    };
    if let Err(err) = store::insert(&conn, &row) {
        // A racing identical PUT can beat us to the row; that is the
        // idempotent case, not a fault. Anything else: the blob is durable,
        // the row is missing, and the next identical PUT heals it — so tell
        // the recorder to retry rather than evict.
        match store::lookup(&conn, filename) {
            Ok(Some(existing)) if existing.sha256 == sha256 => {}
            _ => {
                tracing::error!(%err, "row insert failed after blob write");
                return error(StatusCode::INTERNAL_SERVER_ERROR, "bookkeeping failed");
            }
        }
    }
    receipt(&sha256, body.len())
}

enum WriteOutcome {
    Written,
    AlreadyIdentical,
    AlreadyDivergent,
}

/// Compare an existing blob against incoming bytes — the crash-recovery and
/// lost-race path. Hash equality means "the earlier delivery already stands".
fn compare_existing(dest: &FsPath, body: &[u8]) -> std::io::Result<WriteOutcome> {
    let existing = std::fs::read(dest)?;
    if sha256_hex(&existing) == sha256_hex(body) {
        Ok(WriteOutcome::AlreadyIdentical)
    } else {
        Ok(WriteOutcome::AlreadyDivergent)
    }
}

fn write_blob(
    root: &FsPath,
    dir: &FsPath,
    dest: &FsPath,
    body: &[u8],
) -> std::io::Result<WriteOutcome> {
    if dest.exists() {
        return compare_existing(dest, body);
    }
    std::fs::create_dir_all(dir)?;
    let tmpdir = root.join("ingest").join(".tmp");
    std::fs::create_dir_all(&tmpdir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(&tmpdir)?;
    tmp.write_all(body)?;
    tmp.as_file().sync_all()?;
    match tmp.persist_noclobber(dest) {
        Ok(_) => {}
        Err(err) if err.error.kind() == std::io::ErrorKind::AlreadyExists => {
            return compare_existing(dest, body);
        }
        Err(err) => return Err(err.error),
    }
    // The rename is durable only once the directory entry is — fsync the dir.
    std::fs::File::open(dir)?.sync_all()?;
    Ok(WriteOutcome::Written)
}

pub async fn put_segment(
    State(config): State<Arc<Config>>,
    Path((source, filename)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let name = match names::parse(&source, &filename) {
        Ok(name) => name,
        Err(err) => return error(StatusCode::BAD_REQUEST, err.as_str()),
    };
    if let Err(refused) = write_auth(&config, &headers, &source) {
        return refused.into_response();
    }
    let sent_utc = headers
        .get("x-recall-sent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let handle = tokio::task::spawn_blocking(move || {
        store_segment(&config, &name, &filename, &body, sent_utc)
    });
    match handle.await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(%err, "store task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "store task failed")
        }
    }
}

#[derive(Deserialize)]
pub struct ListParams {
    source: Option<String>,
    since: Option<String>,
    limit: Option<u32>,
}

pub async fn list_segments(
    State(config): State<Arc<Config>>,
    Query(params): Query<ListParams>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    if let Some(source) = &params.source
        && !names::valid_source(source)
    {
        return error(StatusCode::BAD_REQUEST, "invalid source id");
    }
    let limit = params.limit.unwrap_or(1000).min(10_000);
    let handle = tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<Row>> {
        let conn = store::open(&config.root)?;
        store::list(
            &conn,
            params.source.as_deref(),
            params.since.as_deref(),
            limit,
        )
    });
    match handle.await {
        Ok(Ok(rows)) => (StatusCode::OK, axum::Json(json!({ "segments": rows }))).into_response(),
        Ok(Err(err)) => {
            tracing::error!(%err, "listing failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "listing failed")
        }
        Err(err) => {
            tracing::error!(%err, "listing task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "listing failed")
        }
    }
}

/// Liveness for recorders that stream to nothing: each source's newest delivered
/// segment by CAPTURE time. The `.alive` markers the panel was built on are
/// refreshed by a STREAM, so a store-and-forward recorder never touches one and
/// reads dead while recording perfectly (#1428).
pub async fn liveness(State(config): State<Arc<Config>>, headers: HeaderMap) -> Response {
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    let handle = tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<(String, String)>> {
        let conn = store::open(&config.root)?;
        store::liveness(&conn)
    });
    match handle.await {
        Ok(Ok(rows)) => {
            let sources: BTreeMap<String, String> = rows.into_iter().collect();
            (StatusCode::OK, axum::Json(json!({ "sources": sources }))).into_response()
        }
        Ok(Err(err)) => {
            tracing::error!(%err, "liveness query failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "liveness failed")
        }
        Err(err) => {
            tracing::error!(%err, "liveness task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "liveness failed")
        }
    }
}

pub async fn get_blob(
    State(config): State<Arc<Config>>,
    Path((source, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let name = match names::parse(&source, &filename) {
        Ok(name) => name,
        Err(err) => return error(StatusCode::BAD_REQUEST, err.as_str()),
    };
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    let path = config.root.join("ingest").join(&source).join(&filename);
    let handle = tokio::task::spawn_blocking(move || std::fs::read(path));
    match handle.await {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, name.ext.content_type())],
            bytes,
        )
            .into_response(),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            error(StatusCode::NOT_FOUND, "no such segment")
        }
        Ok(Err(err)) => {
            tracing::error!(%err, "blob read failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "blob read failed")
        }
        Err(err) => {
            tracing::error!(%err, "blob task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "blob read failed")
        }
    }
}

/// Stage E1: lease the newest available job. The runner's plane is the sync
/// token's — same trust as reading blobs, which the job points at.
pub async fn lease_job(State(config): State<Arc<Config>>, headers: HeaderMap) -> Response {
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    let handle = tokio::task::spawn_blocking(move || crate::queue::lease(&config.root, Utc::now()));
    match handle.await {
        Ok(Ok(Some(job))) => (StatusCode::OK, axum::Json(json!({ "job": job }))).into_response(),
        Ok(Ok(None)) => (StatusCode::OK, axum::Json(json!({ "job": null }))).into_response(),
        Ok(Err(err)) => {
            tracing::error!(%err, "lease failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "lease failed")
        }
        Err(err) => {
            tracing::error!(%err, "lease task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "lease failed")
        }
    }
}

/// Retire a leased job with its result payload.
pub async fn finish_job(
    State(config): State<Arc<Config>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    let result = String::from_utf8_lossy(&body).into_owned();
    let handle = tokio::task::spawn_blocking(move || {
        crate::queue::done(&config.root, id, &result, Utc::now())
    });
    match handle.await {
        Ok(Ok(true)) => (StatusCode::OK, axum::Json(json!({ "done": true }))).into_response(),
        Ok(Ok(false)) => error(StatusCode::NOT_FOUND, "no such open job"),
        Ok(Err(err)) => {
            tracing::error!(%err, "finish failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "finish failed")
        }
        Err(err) => {
            tracing::error!(%err, "finish task failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "finish failed")
        }
    }
}

pub async fn health() -> Response {
    (StatusCode::OK, axum::Json(json!({ "ok": true }))).into_response()
}
