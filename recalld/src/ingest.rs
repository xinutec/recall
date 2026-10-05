//! The ingest plane (docs/architecture.md): recorders PUT closed segments and
//! get a sha-256 receipt, which they check against their own hash before
//! evicting a local copy.
//!
//! - Durable before acknowledged: temp file, fsync, rename, directory fsync,
//!   then the row, then the receipt. A crash leaves nothing, or a blob the next
//!   identical PUT heals a row for.
//! - Append-only: identical bytes are idempotent; different bytes under a
//!   taken name are 409. There is no delete endpoint (decision 2).
//! - A device token can only write. Reading takes the sync token.

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

/// A refused credential; small, since a `Response` is too large for an `Err`.
struct AuthError {
    status: StatusCode,
    message: &'static str,
}

impl AuthError {
    fn into_response(self) -> Response {
        error(self.status, self.message)
    }
}

/// The bearer must be the source's own token, when tokens are configured.
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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

/// The blocking half of a PUT. 400: fix the name; 409: the name is taken;
/// 500: retry.
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
    let dir = store::source_dir(&config.root, &name.source);
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
        // A racing identical PUT may have written the row. Otherwise the blob
        // is durable without a row: ask for a retry, which heals it.
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
    // The rename is durable only once the directory is.
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

/// Liveness for store-and-forward recorders, which refresh no `.alive`
/// marker: each source's newest delivery, and newest delivery with speech (an
/// unmeasured segment counts; the scanner runs behind).
pub async fn liveness(State(config): State<Arc<Config>>, headers: HeaderMap) -> Response {
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    let handle =
        tokio::task::spawn_blocking(move || -> rusqlite::Result<Vec<(String, String, String)>> {
            let conn = store::open(&config.root)?;
            crate::speech::liveness_by_source(&conn)
        });
    match handle.await {
        Ok(Ok(rows)) => {
            let sources: BTreeMap<String, serde_json::Value> = rows
                .into_iter()
                .map(|(source, delivered, speech)| {
                    let speech = if speech.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(speech)
                    };
                    (source, json!({ "delivered": delivered, "speech": speech }))
                })
                .collect();
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
    let path = store::source_dir(&config.root, &source).join(&filename);
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

/// What a runner says it can do (`?kinds=transcribe-segment,enroll-speaker`).
#[derive(Debug, Deserialize, Default)]
pub struct LeaseQuery {
    kinds: Option<String>,
}

impl LeaseQuery {
    /// Absent means `transcribe-segment` only: an older runner holds only the
    /// `asr` shim. An unknown kind (from a newer runner) is dropped.
    fn kinds(&self) -> Vec<audiocore::job::Kind> {
        match self.kinds.as_deref() {
            None => vec![audiocore::job::Kind::TranscribeSegment],
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .filter_map(|k| {
                    k.parse()
                        .inspect_err(|err| tracing::warn!(%err, "lease query"))
                        .ok()
                })
                .collect(),
        }
    }
}

/// Lease the newest job of a kind the caller can do. Takes the sync token, like
/// the blobs the job points at.
pub async fn lease_job(
    State(config): State<Arc<Config>>,
    Query(query): Query<LeaseQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(refused) = read_auth(&config, &headers) {
        return refused.into_response();
    }
    let kinds = query.kinds();
    let handle =
        tokio::task::spawn_blocking(move || -> rusqlite::Result<Option<crate::queue::Job>> {
            let leased = crate::queue::lease(&config.root, Utc::now(), &kinds)?;
            let Some(mut job) = leased else {
                return Ok(None);
            };
            crate::enrol::attach_spans(&config.root, &mut job)?;
            crate::sessions::attach_language(&config.root, &mut job)?;
            Ok(Some(job))
        });
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
