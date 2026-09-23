//! LAN fallback for the mic heartbeat.
//!
//! Audio goes phone -> Mac over the LAN, but the beat goes phone -> Isis over
//! the VPN, so a phone at home with its tunnel off would record correctly and
//! still read `silent`, then `dead`. This relay lets the beat take the LAN too.
//!
//! ⚠ Not the ingest port, and not gated on the pause: `server::serve` closes
//! its listener while capture is paused, which is exactly when the heartbeat is
//! the only signal there is.
//!
//! Isis stays the single source of truth: this forwards and stores nothing, so
//! two places cannot disagree about which mics are alive.
//!
//! Hand-written HTTP, deliberately: one route, one unauthenticated LAN caller,
//! a handful of requests a month. `axum` and `tokio` are only dev-dependencies
//! here, and the capture daemon should not take them on for this.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// The port the phone apps use for the fallback: the same port the fleet API
/// answers on, so the fallback is the identical request with `host` swapped for
/// `controlHost`.
pub const DEFAULT_RELAY_PORT: u16 = 8000;

/// Where a beat is posted, here and on the fleet.
pub const BEAT_PATH: &str = "/api/devices/heartbeat";
/// Bounded because the caller is unauthenticated: a beat is a few hundred bytes.
pub const MAX_BODY_BYTES: usize = 4096;
const MAX_DEVICE_LEN: usize = 64;
const MAX_HEAD_BYTES: usize = 8192;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(8);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What a phone is allowed to say.
///
/// `at` is absent on purpose: the fleet stamps it, so a beat cannot backdate
/// itself. `viaLan` is the relay's testimony, not the phone's, and is added
/// below.
const FROM_PHONE: &[&str] = &[
    "device",
    "app",
    "version",
    "startedAt",
    "streaming",
    "charging",
    "micOk",
];

/// Why a body was not a beat this relay will pass on.
#[derive(Debug, PartialEq, Eq)]
pub struct Rejected(pub String);

/// The beat to forward: what the phone said, filtered, plus how it arrived.
/// An allowlist rather than a scrub, since the caller is unauthenticated: a key
/// not named here cannot reach Isis.
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

/// POST one beat to the fleet. Returns whether it landed.
///
/// Best-effort: the phone learns from the status returned, and its own retry
/// backoff decides what to do.
pub fn forward(beat: &serde_json::Value, fleet_url: &str) -> bool {
    let url = format!("{}{BEAT_PATH}", fleet_url.trim_end_matches('/'));
    // Its own agent: `ureq`'s bare functions share one process-wide connection
    // pool, and a stale pooled socket is an unreproducible 502.
    let agent = ureq::AgentBuilder::new()
        .timeout(FORWARD_TIMEOUT)
        .max_idle_connections(0)
        .build();
    // `send_bytes` with the header set by hand: ureq is built without the
    // `json` feature that provides `send_json`.
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

/// Read the request line and headers, bounded.
///
/// Bounded on both line count and byte count, so an unauthenticated caller
/// sending headers forever costs nothing.
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

/// Answer with an empty body. Success is 204 (the relay has nothing to
/// return); a failed forward is 502, never 204, so "the Mac took it" never
/// reads as "the fleet knows".
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

/// Accept beats on the LAN and pass them to the fleet, forever.
///
/// One thread per connection: a phone whose socket stalls must not hold up the
/// next phone's beat, and the volume is too low to justify anything cleverer.
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
