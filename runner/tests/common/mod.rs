//! What the binary-driving tests share: a real recalld, a queued clip, and a
//! job's row.

use audiocore::instant::IngestStamp;
use recalld::app::{Config as ServerConfig, router};
use std::path::Path;
use std::sync::Arc;
use transcript::SourceId;

pub fn serve(root: &Path, token: &str) -> String {
    recalld::meaning_schema::ensure(
        &rusqlite::Connection::open(root.join("recall.sqlite")).expect("meaning"),
    )
    .expect("schema");
    recalld::store::open(root).expect("ingest db");
    let config = Arc::new(ServerConfig {
        root: root.to_owned(),
        tokens: None,
        read_token: Some(token.to_owned()),
        max_body_bytes: 16 * 1024 * 1024,
        trusted_proxies: Vec::new(),
        webauth: None,
        sync_token: Some(token.to_owned()),
        frontend: None,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr")).expect("send");
            axum::serve(listener, router(config)).await.expect("serve");
        });
    });
    format!("http://{}", rx.recv().expect("addr"))
}

pub fn queued_clip(root: &Path, source: &str, name: &str) {
    let dir = root.join("ingest").join(source);
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join(name), b"audio bytes").expect("blob");
    let conn = recalld::store::open(root).expect("db");
    recalld::store::insert(
        &conn,
        &recalld::store::Row {
            source: SourceId::parse(source).expect("source id"),
            filename: name.to_owned(),
            start_utc: IngestStamp::parse("2026-10-10T10:00:00Z").expect("instant"),
            bytes: 11,
            sha256: "x".to_owned(),
            received_utc: IngestStamp::parse("2026-10-10T10:01:00Z").expect("instant"),
            sent_utc: None,
        },
    )
    .expect("row");
    conn.execute(
        "INSERT INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, '2026-10-10T10:01:00Z')",
        (audiocore::job::Kind::TranscribeSegment, name),
    )
    .expect("job");
}

/// The job's state and stored result.
pub fn job(root: &Path, name: &str) -> (String, Option<String>) {
    rusqlite::Connection::open(root.join("ingest.sqlite"))
        .expect("db")
        .query_row(
            "SELECT state, result FROM jobs WHERE filename = ?1",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("job row")
}
