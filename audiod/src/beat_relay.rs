//! LAN fallback for the mic heartbeat.
//!
//! Audio goes phone -> Mac over the LAN, but the beat goes phone -> Isis over
//! the VPN: a phone at home with its tunnel off would read dead. This relay
//! lets the beat take the LAN too.
//!
//! Not the ingest port and not gated on the pause: the ingest listener closes
//! while paused, exactly when the heartbeat is the only signal. It forwards
//! and stores nothing, so Isis stays the one record.
//!
//! Hand-written HTTP: one route, a handful of requests a month; not worth axum
//! and tokio in the capture daemon.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// The fleet API's port, so the fallback is the same request with `host`
/// swapped for `controlHost`.
pub const DEFAULT_RELAY_PORT: u16 = 8000;

/// Where a beat is posted, here and on the fleet.
pub const BEAT_PATH: &str = "/api/devices/heartbeat";
/// Bounded because the caller is unauthenticated: a beat is a few hundred bytes.
pub const MAX_BODY_BYTES: usize = 4096;
const MAX_DEVICE_LEN: usize = 64;
const MAX_HEAD_BYTES: usize = 8192;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(8);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What a phone may say. No `at`: the fleet stamps it. `viaLan` is the relay's
/// to add.
const FROM_PHONE: &[&str] = &[
    "device",
    "app",
    "version",
    "startedAt",
    "streaming",
    "charging",
    "micOk",
    "droppedBytes",
];

/// Why a body was not a beat this relay will pass on.
#[derive(Debug, PartialEq, Eq)]
pub struct Rejected(pub String);

/// The beat to forward: an allowlist of what the phone said, plus how it
/// arrived. The caller is unauthenticated.
///
/// # Errors
/// If the body is oversized, is not a JSON object, or names no usable device.
pub fn relayed(raw: &[u8]) -> Result<serde_json::Value, Rejected> {
    if raw.len() > MAX_BODY_BYTES {
        return Err(Rejected(format!("body is {} bytes", raw.len())));
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| Rejected(format!("not JSON: {e}")))?;
    let Some(obj) = parsed.as_object() else {
        return Err(Rejected("not an object".to_owned()));
    };
    let device = obj.get("device").and_then(serde_json::Value::as_str);
    match device {
        None | Some("") => return Err(Rejected("no device".to_owned())),
        Some(d) if d.chars().count() > MAX_DEVICE_LEN => {
            return Err(Rejected(format!(
                "device name is {} chars",
                d.chars().count()
            )));
        }
        Some(_) => {}
    }
    let mut out = serde_json::Map::new();
    for key in FROM_PHONE {
        if let Some(v) = obj.get(*key) {
            out.insert((*key).to_owned(), v.clone());
        }
    }
    // Stamped, never copied.
    out.insert("viaLan".to_owned(), serde_json::Value::Bool(true));
    Ok(serde_json::Value::Object(out))
}

/// POST one beat to the fleet; whether it landed. The phone's own backoff
/// handles failure.
pub fn forward(beat: &serde_json::Value, fleet_url: &str) -> bool {
    let url = format!("{}{BEAT_PATH}", fleet_url.trim_end_matches('/'));
    // Its own agent: a stale socket in ureq's shared pool is a random 502.
    let agent = ureq::AgentBuilder::new()
        .timeout(FORWARD_TIMEOUT)
        .max_idle_connections(0)
        .build();
    // ureq is built without `json`.
    let body = beat.to_string();
    match agent
        .post(&url)
        .set("Content-Type", "application/json")
        .send_bytes(body.as_bytes())
    {
        Ok(_) => true,
        Err(ureq::Error::Status(status, _)) => {
            tracing::warn!(status, "beat-relay: the fleet refused a beat");
            false
        }
        Err(err) => {
            tracing::warn!(%err, url, "beat-relay: could not forward a beat");
            false
        }
    }
}

/// The parsed head of one HTTP request: enough for one route and no more.
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    pub content_length: usize,
}

/// Read the request line and headers, bounded in lines and bytes.
///
/// # Errors
/// If the stream ends, the head is oversized, or the request line is not
/// `METHOD PATH VERSION`.
pub fn read_head<R: BufRead>(reader: &mut R) -> Result<Head, Rejected> {
    let mut line = String::new();
    let mut taken = 0usize;
    reader
        .read_line(&mut line)
        .map_err(|e| Rejected(format!("no request line: {e}")))?;
    taken += line.len();
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(Rejected("malformed request line".to_owned()));
    };
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        let n = reader
            .read_line(&mut header)
            .map_err(|e| Rejected(format!("bad header: {e}")))?;
        if n == 0 {
            return Err(Rejected("head ended without a blank line".to_owned()));
        }
        taken += n;
        if taken > MAX_HEAD_BYTES {
            return Err(Rejected(format!("head is over {MAX_HEAD_BYTES} bytes")));
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value
                .trim()
                .parse()
                .map_err(|_| Rejected("unparseable Content-Length".to_owned()))?;
        }
    }
    Ok(Head {
        method,
        path,
        content_length,
    })
}

/// 204 when the fleet took it, 502 when not: the Mac taking it is not the
/// fleet knowing.
fn respond(stream: &mut TcpStream, status: u16, reason: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.flush();
}

fn handle(stream: &mut TcpStream, fleet_url: &str) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(READ_TIMEOUT));
    let peer = stream
        .peer_addr()
        .map_or_else(|_| "?".to_owned(), |a| a.to_string());
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!(%err, "beat-relay: cannot read the connection");
            return;
        }
    });
    let head = match read_head(&mut reader) {
        Ok(head) => head,
        Err(Rejected(why)) => {
            tracing::warn!(why, peer, "beat-relay: bad request");
            respond(stream, 400, "Bad Request");
            return;
        }
    };
    if head.method != "POST" || head.path != BEAT_PATH {
        respond(stream, 404, "Not Found");
        return;
    }
    if head.content_length > MAX_BODY_BYTES {
        respond(stream, 413, "Payload Too Large");
        return;
    }
    let mut body = vec![0u8; head.content_length];
    if reader.read_exact(&mut body).is_err() {
        respond(stream, 400, "Bad Request");
        return;
    }
    let beat = match relayed(&body) {
        Ok(beat) => beat,
        Err(Rejected(why)) => {
            tracing::warn!(why, peer, "beat-relay: refused a beat");
            respond(stream, 400, "Bad Request");
            return;
        }
    };
    if !forward(&beat, fleet_url) {
        respond(stream, 502, "Bad Gateway");
        return;
    }
    let device = beat.get("device").and_then(serde_json::Value::as_str);
    tracing::info!(device, "beat-relay: relayed a beat");
    respond(stream, 204, "No Content");
}

/// Accept beats on the LAN and pass them on, one thread per connection so a
/// stalled socket holds up nobody.
pub fn serve(port: u16, fleet_url: &str) -> std::io::Error {
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(err) => return err,
    };
    tracing::info!(port, fleet_url, "beat-relay: listening");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let fleet = fleet_url.to_owned();
                std::thread::spawn(move || handle(&mut stream, &fleet));
            }
            Err(err) => tracing::warn!(%err, "beat-relay: accept failed"),
        }
    }
    std::io::Error::other("the accept loop ended")
}
