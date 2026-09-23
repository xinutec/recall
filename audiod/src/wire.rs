//! The device-ingest wire protocol: the few facts both ends must agree on.
//!
//! The phone clients (`android/.../Handshake.kt`, `ios/Sources/Handshake.swift`)
//! emit the same one-line JSON; `tests/handshakes.json` holds copies of what
//! they send, so a parser change that would strand a mic fails there first.

use std::io::Read;

/// The one shared port every device connects to.
pub const DEFAULT_INGEST_PORT: u16 = 9999;

/// The PCM the pipeline speaks end to end: 48 kHz signed 16-bit little-endian.
pub const SAMPLE_RATE: u32 = 48_000;
pub const BYTES_PER_SAMPLE: u32 = 2;

const DEFAULT_CHANNELS: u16 = 1;
const MAX_HANDSHAKE_BYTES: usize = 8192;

/// A device's opening announcement: who it is + its PCM format.
///
/// `epoch` (optional) is the phone's wall clock, in unix seconds, at the first
/// PCM byte it streams, which lets the server shift arrival-stamped segment
/// names back to capture time. `None` when absent or unreadable: a bad epoch
/// degrades to arrival-stamping, never to a dropped stream.
#[derive(Debug, Clone, PartialEq)]
pub struct Handshake {
    pub source_id: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub epoch: Option<f64>,
}

/// A JSON value read as an integer: integers pass, floats truncate toward zero,
/// digit strings parse. Anything else is malformed.
fn as_int(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parse the handshake `{"id":"kitchen","rate":48000,"channels":1}`.
/// rate/channels default to 48k mono. `None` if malformed, the id is not a
/// valid source id (it becomes a directory name), or the format is non-positive.
pub fn parse_handshake(line: &str) -> Option<Handshake> {
    let data: serde_json::Value = serde_json::from_str(line).ok()?;
    let source_id = match data.get("id")? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if !audiocore::names::valid_source(&source_id) {
        return None;
    }
    let rate = match data.get("rate") {
        Some(v) => as_int(v)?,
        None => i64::from(SAMPLE_RATE),
    };
    let channels = match data.get("channels") {
        Some(v) => as_int(v)?,
        None => i64::from(DEFAULT_CHANNELS),
    };
    if rate <= 0 || channels <= 0 {
        return None;
    }
    // Tolerated when unreadable: see Handshake.epoch.
    let epoch = data.get("epoch").and_then(serde_json::Value::as_f64);
    Some(Handshake {
        source_id,
        sample_rate: rate as u32,
        channels: channels as u16,
        epoch,
    })
}

/// Every way of not getting a handshake, so the server can log which: a silent
/// port scanner and a phone that died mid-connect leave different traces.
#[derive(Debug)]
pub enum HandshakeError {
    /// The peer closed before the newline.
    Eof,
    /// The read failed — a timeout on a silent peer, mostly.
    Io(std::io::Error),
    /// No newline within the size cap.
    Overflow,
    /// The line arrived but didn't parse (or the id was unsafe).
    Malformed,
}

/// Read exactly the newline-terminated handshake line, byte by byte so not one
/// byte of the PCM that follows is consumed.
pub fn read_handshake(reader: &mut impl Read) -> Result<Handshake, HandshakeError> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < MAX_HANDSHAKE_BYTES {
        match reader.read(&mut byte) {
            Ok(0) => return Err(HandshakeError::Eof),
            Err(err) => return Err(HandshakeError::Io(err)),
            Ok(_) => {}
        }
        if byte[0] == b'\n' {
            return parse_handshake(&String::from_utf8_lossy(&buf))
                .ok_or(HandshakeError::Malformed);
        }
        buf.push(byte[0]);
    }
    Err(HandshakeError::Overflow)
}
