//! Report recall's health to fleetwatch (its README, "The report contract").
//! fleetwatch turns a producer that stops reporting red, so a dead Mac needs
//! no detector of its own.

use crate::check::Check;
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_URL: &str = "https://fleetwatch.xinutec.org/api/reports";
/// One collector for all of recall: one tile.
const COLLECTOR: &str = "recall";
/// The declared cadence, from which fleetwatch judges staleness. Must match
/// the launchd agent's `StartInterval`.
const INTERVAL_S: u32 = 300;
const SCHEMA: u32 = 1;
/// Crockford base32: no I, L, O, U.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A ULID: 48 bits of millisecond timestamp, then 80 bits of randomness, in
/// Crockford base32. fleetwatch's idempotency key; it refuses anything else.
pub fn mint_ulid(now: DateTime<Utc>, randomness: [u8; 10]) -> String {
    let value = ((now.timestamp_millis() as u128) << 80)
        | u128::from_be_bytes({
            let mut wide = [0u8; 16];
            wide[6..].copy_from_slice(&randomness);
            wide
        });
    (0..26)
        .map(|i| {
            let shift = 125 - i * 5;
            CROCKFORD[((value >> shift) & 0x1F) as usize] as char
        })
        .collect()
}

/// 80 bits from `/dev/urandom`; zeros if it cannot be read. An idempotency
/// key, not a secret.
fn randomness() -> [u8; 10] {
    use std::io::Read as _;
    let mut bytes = [0u8; 10];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    bytes
}

/// The report body. No `source`: fleetwatch takes it from the token.
pub fn payload(
    checks: &[Check],
    now: DateTime<Utc>,
    duration_ms: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "schema": SCHEMA,
        "id": mint_ulid(now, randomness()),
        "collector": COLLECTOR,
        "collected_at": now.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
        "duration_ms": duration_ms,
        "interval_s": INTERVAL_S,
        "checks": checks,
    })
}

/// The ingest token: `from_env` (`RECALL_FLEETWATCH_TOKEN`), else
/// `~/.config/fleetwatch/token`. Passed in so tests need not set the
/// environment.
pub fn read_token(home: &Path, from_env: Option<&str>) -> Option<String> {
    let env = from_env.map(str::trim).filter(|t| !t.is_empty());
    if let Some(token) = env {
        return Some(token.to_owned());
    }
    let path = home.join(".config").join("fleetwatch").join("token");
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

/// POST one report. Returns the HTTP status (201 stored, 200 duplicate).
pub fn post(body: &serde_json::Value, token: &str, url: &str) -> Result<u16, Box<ureq::Error>> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let response = agent
        .post(url)
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {token}"))
        .send_json(body)
        .map_err(Box::new)?;
    Ok(response.status())
}
