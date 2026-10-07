//! The reads the web app and the CLI make, replayed against a database copy and
//! stored, so a change to how reads are served can show it answers the same.
//!
//! `take --root LAB --out DIR` mounts the router signed in and writes one file
//! per request. Later requests take their cursors and ids from earlier answers,
//! so two runs over one copy ask the same questions. `diff A B` prints, per
//! request, whether the answer is the same, and if not, which JSON paths
//! differ: never values. The stored answers hold transcript text: keep DIR
//! beside the copy and delete them together.
//!
//! Usage: `cargo run --release --example api_snapshot -- take --root LAB --out DIR`
//!        `cargo run --release --example api_snapshot -- diff A B`

use axum::body::Body;
use axum::http::Request;
use recalld::app::{Config, DEFAULT_MAX_BODY, router};
use recalld::webauth::{self, COOKIE_NAME, GateState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

const SECRET: &str = "api-snapshot-not-a-real-secret";
/// The web app's page sizes: timeline and Check.
const TIMELINE_PAGE: usize = 200;
const CHECK_PAGE: usize = 1000;
/// Lines whose transcripts and audio are fetched: the first, then every Nth.
const SAMPLE_FIRST: usize = 40;
const SAMPLE_EVERY: usize = 500;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(clap::Parser)]
enum Command {
    /// Replay the reads against a copy and store the answers.
    Take {
        /// The copy's data root.
        #[arg(long)]
        root: PathBuf,
        /// Where the answers go, one file per request.
        #[arg(long)]
        out: PathBuf,
    },
    /// Compare two stored runs, by JSON path.
    Diff { a: PathBuf, b: PathBuf },
}

fn main() -> Result<()> {
    match <Command as clap::Parser>::parse() {
        Command::Take { root, out } => tokio::runtime::Runtime::new()?.block_on(take(&root, &out)),
        Command::Diff { a, b } => diff(&a, &b),
    }
}

struct Client {
    app: axum::Router,
    cookie: String,
    out: PathBuf,
    taken: usize,
}

impl Client {
    /// One request, stored; its JSON body if it has one.
    async fn get(&mut self, path: &str) -> Result<Option<Value>> {
        let response = self
            .app
            .clone()
            .oneshot(
                Request::get(path)
                    .header("cookie", format!("{COOKIE_NAME}={}", self.cookie))
                    .body(Body::empty())?,
            )
            .await?;
        let status = response.status().as_u16();
        let kind = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 30).await?;
        let parsed: Option<Value> = kind
            .starts_with("application/json")
            .then(|| serde_json::from_slice(&bytes).ok())
            .flatten();
        let body = match (&parsed, kind.as_str()) {
            (Some(value), _) => value.clone(),
            (None, "audio/wav") => wav_summary(&bytes),
            (None, _) => {
                json!({"bytes": bytes.len(), "sha256": hex::encode(Sha256::digest(&bytes))})
            }
        };
        self.taken += 1;
        let record = json!({"request": path, "status": status, "type": kind, "body": body});
        std::fs::write(
            self.out.join(format!("{:05}.json", self.taken)),
            serde_json::to_vec(&record)?,
        )?;
        Ok(parsed.filter(|_| status == 200))
    }
}

/// A clip as what can be heard: its shape and its loudness per 100 ms, in whole
/// dB floored at -60. Not its bytes: `sox norm` dithers, so no two renders of
/// one clip are byte-identical.
fn wav_summary(bytes: &[u8]) -> Value {
    let u16_at = |i: usize| {
        bytes
            .get(i..i + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let u32_at = |i: usize| {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let (mut at, mut format, mut data) = (12, None, None);
    while let (Some(id), Some(size)) = (bytes.get(at..at + 4), u32_at(at + 4)) {
        let body = at + 8;
        match id {
            b"fmt " => format = u16_at(body + 2).zip(u32_at(body + 4)),
            b"data" => data = bytes.get(body..(body + size as usize).min(bytes.len())),
            _ => {}
        }
        at = body + size as usize + (size as usize & 1);
    }
    let (Some((channels, rate)), Some(data)) = (format, data) else {
        return json!({"unreadable_wav_bytes": bytes.len()});
    };
    let samples: Vec<f64> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| f64::from(i16::from_le_bytes(*s)) / 32768.0)
        .collect();
    let window = (rate as usize / 10 * usize::from(channels)).max(1);
    let loudness: Vec<i64> = samples
        .chunks(window)
        .map(|w| {
            let rms = (w.iter().map(|x| x * x).sum::<f64>() / w.len() as f64).sqrt();
            #[expect(clippy::cast_possible_truncation, reason = "whole dB in -60..=0")]
            let db = (20.0 * rms.max(1e-3).log10()).round() as i64;
            db
        })
        .collect();
    json!({"rate": rate, "channels": channels, "samples": samples.len(), "loudness_db": loudness})
}

fn query(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

fn strings(value: &Value, pointer: &str, field: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| i.get(field).and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Every shown line's id on a conversation page, in order.
fn line_ids(page: &Value) -> Vec<i64> {
    page.pointer("/items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|c| {
            c.get("moments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|m| m.pointer("/primary/id").and_then(Value::as_i64))
        .collect()
}

/// Page back through `/api/conversations` as the app does, from the newest.
async fn page_back(
    client: &mut Client,
    page: usize,
    max_pages: usize,
    ids: &mut Vec<i64>,
) -> Result<()> {
    let limit = page.to_string();
    let mut before: Option<String> = None;
    for _ in 0..max_pages {
        let mut pairs = vec![("limit", limit.as_str())];
        if let Some(b) = &before {
            pairs.push(("before", b.as_str()));
        }
        let Some(answer) = client
            .get(&format!("/api/conversations?{}", query(&pairs)))
            .await?
        else {
            break;
        };
        ids.extend(line_ids(&answer));
        let oldest = strings(&answer, "/items", "start").into_iter().min();
        let more = answer.get("hasMore").and_then(Value::as_bool) == Some(true);
        match oldest {
            Some(start) if more && before.as_deref() != Some(&start) => before = Some(start),
            _ => break,
        }
    }
    Ok(())
}

/// The router over `root`, mounted behind a gate this run signs itself into.
fn signed_in(root: &Path, out: &Path) -> Result<Client> {
    let now = chrono::Utc::now().timestamp();
    let gate = GateState {
        cfg: Arc::new(webauth::Config {
            session_secret: SECRET.into(),
            client_id: "snapshot".into(),
            client_secret: "snapshot".into(),
            nc_base_url: "https://nextcloud.invalid".into(),
            nc_internal_url: "https://nextcloud.invalid".into(),
            redirect_uri: "https://recall.invalid/auth/callback".into(),
            allowed_users: std::collections::HashSet::new(),
            device_token: None,
        }),
        now: Arc::new(move || now),
    };
    let cookie = webauth::make_session_cookie(
        SECRET,
        &webauth::Session {
            user_id: "snapshot".into(),
            display_name: "snapshot".into(),
        },
        now,
    )
    .ok_or("cannot sign a session")?;
    let app = router(Arc::new(Config {
        root: root.to_path_buf(),
        tokens: None,
        read_token: None,
        max_body_bytes: DEFAULT_MAX_BODY,
        trusted_proxies: Vec::new(),
        webauth: Some(gate),
        sync_token: None,
        frontend: None,
    }));
    Ok(Client {
        app,
        cookie,
        out: out.to_path_buf(),
        taken: 0,
    })
}

async fn take(root: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let mut client = signed_in(root, out)?;
    // Timeline and Check: the whole archive, a page at a time.
    let mut ids = Vec::new();
    page_back(&mut client, TIMELINE_PAGE, 100_000, &mut ids).await?;
    page_back(&mut client, CHECK_PAGE, 3, &mut Vec::new()).await?;
    // The CLI's own reads.
    client.get("/api/timeline?limit=1000").await?;
    client.get("/api/review?limit=50").await?;
    sessions(&mut client).await?;
    let names = labels(&mut client).await?;
    search(&mut client, names).await?;
    lines(&mut client, &ids).await?;
    println!("{} requests stored in {}", client.taken, out.display());
    Ok(())
}

/// Each session read through, with hidden lines and as exported.
async fn sessions(client: &mut Client) -> Result<()> {
    if let Some(list) = client.get("/api/sessions").await? {
        for id in strings(&list, "/items", "id") {
            for hidden in [false, true] {
                let mut pairs = vec![("source", id.as_str()), ("limit", "5000")];
                if hidden {
                    pairs.push(("hidden", "true"));
                }
                client
                    .get(&format!("/api/conversations?{}", query(&pairs)))
                    .await?;
            }
            let path: String = form_urlencoded::byte_serialize(id.as_bytes()).collect();
            client
                .get(&format!("/api/sessions/{path}/transcript"))
                .await?;
        }
    }
    Ok(())
}

/// Speakers, their corrections, and a sample of correction audio; the names.
async fn labels(client: &mut Client) -> Result<Vec<String>> {
    let names = match client.get("/api/speakers").await? {
        Some(s) => s
            .get("names")
            .and_then(Value::as_array)
            .map(|n| {
                n.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::<String>::new(),
    };
    let mut labels: Vec<i64> = Vec::new();
    if let Some(all) = client.get("/api/corrections?limit=200").await? {
        labels.extend(
            all.pointer("/items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("id").and_then(Value::as_i64)),
        );
    }
    for name in &names {
        client
            .get(&format!(
                "/api/corrections?{}",
                query(&[("speaker", name), ("limit", "200")])
            ))
            .await?;
    }
    for id in labels.iter().take(20) {
        client.get(&format!("/api/correction/{id}/audio")).await?;
        client
            .get(&format!("/api/correction/{id}/audio?context=true"))
            .await?;
    }
    Ok(names)
}

/// Every vocabulary term and every speaker name, searched for.
async fn search(client: &mut Client, names: Vec<String>) -> Result<()> {
    let mut terms = match client.get("/api/vocabulary").await? {
        Some(v) => strings(&v, "/items", "term"),
        None => Vec::new(),
    };
    terms.extend(names);
    for term in &terms {
        client
            .get(&format!(
                "/api/search?{}",
                query(&[("q", term), ("limit", "100")])
            ))
            .await?;
    }
    Ok(())
}

/// A sample of the timeline's lines: their transcripts and their audio.
async fn lines(client: &mut Client, ids: &[i64]) -> Result<()> {
    let picked: Vec<usize> = (0..ids.len())
        .filter(|i| *i < SAMPLE_FIRST || i % SAMPLE_EVERY == 0)
        .collect();
    let sample: Vec<i64> = picked.iter().map(|&i| ids[i]).collect();
    for chunk in sample.chunks(50) {
        let joined = chunk
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        client
            .get(&format!("/api/transcripts?{}", query(&[("ids", &joined)])))
            .await?;
    }
    for id in &sample {
        client.get(&format!("/api/audio/{id}")).await?;
        client.get(&format!("/api/audio/{id}?pad=1")).await?;
    }
    // Each sampled line with the next one on the timeline.
    for &i in picked.iter().filter(|&&i| i % SAMPLE_EVERY == 0) {
        if let Some(next) = ids.get(i + 1) {
            client
                .get(&format!("/api/audio-span?from_id={}&to_id={next}", ids[i]))
                .await?;
        }
    }
    Ok(())
}

fn load(dir: &Path) -> Result<BTreeMap<String, Value>> {
    let mut by_request = BTreeMap::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "json") {
            let record: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            let request = record["request"].as_str().unwrap_or_default().to_owned();
            by_request.insert(request, record);
        }
    }
    Ok(by_request)
}

/// The JSON paths where `a` and `b` differ, at most `room` of them; array
/// elements by index, object fields by name.
fn differing(a: &Value, b: &Value, at: &str, found: &mut Vec<String>, room: usize) {
    if found.len() >= room || a == b || (at.ends_with("/loudness_db") && within_a_db(a, b)) {
        return;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let keys: std::collections::BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for key in keys {
                let null = Value::Null;
                differing(
                    x.get(key).unwrap_or(&null),
                    y.get(key).unwrap_or(&null),
                    &format!("{at}/{key}"),
                    found,
                    room,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                differing(p, q, &format!("{at}/{i}"), found, room);
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            found.push(format!("{at} (length {} vs {})", x.len(), y.len()));
        }
        _ => found.push(at.to_owned()),
    }
}

/// Dither can tip a window across a half-dB boundary.
fn within_a_db(a: &Value, b: &Value) -> bool {
    match (a.as_array(), b.as_array()) {
        (Some(x), Some(y)) => {
            x.len() == y.len()
                && x.iter().zip(y).all(|(p, q)| {
                    p.as_i64()
                        .zip(q.as_i64())
                        .is_some_and(|(p, q)| p.abs_diff(q) <= 1)
                })
        }
        _ => false,
    }
}

/// Request paths with ids and cursors folded, so a report names its kind only.
fn kind_of(request: &str) -> String {
    let path = request.split('?').next().unwrap_or(request);
    path.split('/')
        // A line, label or session id: any part with a digit in it.
        .map(|part| {
            if part.chars().any(|c| c.is_ascii_digit()) {
                "{id}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn diff(a: &Path, b: &Path) -> Result<()> {
    let (a, b) = (load(a)?, load(b)?);
    let mut same: BTreeMap<String, usize> = BTreeMap::new();
    let mut changed: BTreeMap<String, usize> = BTreeMap::new();
    let mut paths: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut only = (0, 0);
    for (request, left) in &a {
        let kind = kind_of(request);
        let Some(right) = b.get(request) else {
            only.0 += 1;
            continue;
        };
        let mut found = Vec::new();
        differing(left, right, "", &mut found, 20);
        if found.is_empty() {
            *same.entry(kind).or_default() += 1;
            continue;
        }
        *changed.entry(kind.clone()).or_default() += 1;
        for path in found {
            // Indexes folded too: which field, not which element.
            let general = path
                .split('/')
                .map(|p| {
                    if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() {
                        "*"
                    } else {
                        p
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            *paths
                .entry(kind.clone())
                .or_default()
                .entry(general)
                .or_default() += 1;
        }
    }
    only.1 = b.keys().filter(|r| !a.contains_key(*r)).count();
    println!("same:    {same:?}");
    println!("changed: {changed:?}");
    println!("requests only in A: {}, only in B: {}", only.0, only.1);
    for (kind, fields) in &paths {
        println!("{kind}");
        for (field, n) in fields {
            println!("  {n:6}  {field}");
        }
    }
    Ok(())
}
