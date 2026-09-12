//! LAN fallback for the mic heartbeat. Port of `src/recall/beat_relay.py` (#888).
//!
//! WHY THIS EXISTS. The beat's reachability requirement used to be STRICTER than
//! recording's, which made the check lie about working phones:
//!
//! ```text
//! audio   phone -> Mac,  192.168.1.81:9999   (LAN)
//! beat    phone -> Isis, 10.100.0.2:8000     (VPN)
//! ```
//!
//! A phone at home with its tunnel off streamed every sample correctly and still
//! read `silent` after 3 h and `dead` after 12 h. Both false. Measured twice on
//! 2026-08-14, and still in use: iphone11, oneplus6t and pixel5 have each
//! relayed beats this way within the last month.
//!
//! ⚠ **NOT the ingest port, and not gated on the pause.** `server::serve` closes
//! its listener while capture is paused — and a pause is exactly when the
//! heartbeat is the only signal there is. A beat receiver has to be independent
//! of the capture lifecycle, so this is its own listener that runs whatever
//! capture is doing.
//!
//! ⚠ **ISIS REMAINS THE SINGLE SOURCE OF TRUTH.** This forwards; it does not
//! store. A local beat store that the collector had to merge with the fleet's
//! would let two places disagree about which mics are alive, which is worse than
//! the bug being fixed.
//!
//! ⚠ **Hand-written HTTP, deliberately.** One route, one method, an
//! unauthenticated LAN caller and about fifteen requests a month. `axum` and
//! `tokio` are dev-dependencies of this crate so the upload tests can run
//! against the real recalld router; promoting them to real dependencies of the
//! capture daemon — the one process that must never die — to serve this would be
//! the wrong trade. The accept loop follows `server.rs`, which already does this
//! by hand for the ingest port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

/// Where a relayed beat goes on the fleet.
/// The port the phone apps use for the fallback.
///
/// ⚠ The same port the FLEET API answers on, so the apps need no second URL
/// shape: the fallback is the identical request with `host` swapped for
/// `controlHost`. A different machine, so nothing collides.
pub const DEFAULT_RELAY_PORT: u16 = 8000;

pub const BEAT_PATH: &str = "/api/devices/heartbeat";
/// Bounded because the caller is unauthenticated: a beat is a few hundred bytes.
pub const MAX_BODY_BYTES: usize = 4096;
const MAX_DEVICE_LEN: usize = 64;
const MAX_HEAD_BYTES: usize = 8192;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(8);
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// What a phone is allowed to say.
///
/// ⚠ `at` is absent ON PURPOSE — the fleet stamps it from its own clock, so a
/// beat cannot backdate itself. `viaLan` is absent because it is this relay's
/// testimony, not the phone's, and is added below.
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

/// The beat to forward: what the phone said, FILTERED, plus how it arrived.
///
/// This is the boundary between an unauthenticated LAN caller and the fleet's
/// store, so it is an allowlist rather than a scrub: a key nobody named here
/// cannot reach Isis by being added to the app.
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
/// Best-effort like every other part of this path: a relay that raised its own
/// failures would be the tail wagging the dog. The phone learns from the status
/// we return, and its own retry backoff decides what to do about it.
pub fn forward(beat: &serde_json::Value, fleet_url: &str) -> bool {
    let url = format!("{}{BEAT_PATH}", fleet_url.trim_end_matches('/'));
    // Its own agent: `ureq`'s bare functions share ONE process-wide connection
    // pool, and a relay that hands a stale pooled socket to a phone's beat is a
    // 502 nobody can reproduce (recalld's proxy paid for this once).
    let agent = ureq::AgentBuilder::new()
        .timeout(FORWARD_TIMEOUT)
        .max_idle_connections(0)
        .build();
    // `send_bytes` with the header set by hand: this crate takes `ureq` without
    // default features, so the `json` feature that would provide `send_json` is
    // not compiled in — and adding it to post one small object would pull serde
    // into ureq for no gain.
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
/// ⚠ Bounded on BOTH the line count and the byte count. An unauthenticated
/// caller that opens a socket and sends headers forever must cost this process
/// nothing, and "read until a blank line" alone is that hole.
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

/// What this relay answers, and why each one rather than the neighbouring code.
///
/// ⚠ **204 for success, not 200.** The relay stores nothing, so it has no body
/// to return — and the fleet answers 200, so insisting on that would have made
/// every relayed beat read as a failure to the phone that sent it.
///
/// ⚠ **502 when the forward fails, never 204.** The phone must not read "the Mac
/// took it" as "the fleet knows", or a dead Isis would look like three healthy
/// mics.
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
/// One thread per connection, like the Python's `ThreadingHTTPServer`: a phone
/// whose socket stalls must not hold up the next phone's beat, and the volume
/// (about fifteen a month) makes anything cleverer unjustifiable.
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
