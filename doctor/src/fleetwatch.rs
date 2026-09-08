//! Report recall's health to fleetwatch — the fleet's monitoring platform.
//!
//! fleetwatch is push-based: a producer POSTs verdict-shaped reports and it
//! keeps their history (see its README, "The report contract"). Two properties
//! make it the right place for this, and they are why nothing here needs to
//! send mail or run a daemon that watches a daemon:
//!
//! * **A dead producer is a failure, not a silence.** A report declares its own
//!   cadence (`interval_s`); fleetwatch renders a producer that has stopped
//!   reporting as `Silent` — red. So this need not detect the case where the
//!   *Mac itself* dies: not reporting IS the report. A monitor that only speaks
//!   when it is well is no monitor at all.
//! * The verdicts already show up where the rest of the fleet's health does,
//!   which is where a fault will actually be seen.
//!
//! The ingest token is read from `~/.config/fleetwatch/token` and never leaves
//! this machine — it is not in the repo, not in the report, and not logged.

use crate::check::Check;
use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_URL: &str = "https://fleetwatch.xinutec.org/api/reports";
/// One producer for the whole of recall: is it recording, are its agents up, is
/// the archive mirrored. Kept as one collector so a single fleetwatch tile
/// answers "is recall alright?" — the question actually being asked.
const COLLECTOR: &str = "recall";
/// Declared cadence. fleetwatch turns this into staleness: report less often
/// than this and the tile goes amber, stop entirely and it goes red. It must
/// match the launchd agent's `StartInterval`, or a healthy producer is reported
/// as late.
const INTERVAL_S: u32 = 300;
const SCHEMA: u32 = 1;
/// A ULID alphabet — no I, L, O, U.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A ULID: 48 bits of millisecond timestamp, then 80 bits of randomness, in
/// Crockford base32. fleetwatch uses it as the idempotency key and rejects
/// anything that is not one (422), so it is minted here rather than depending
/// on a crate for a dozen lines. Pure, so it is tested against a known vector.
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

/// 80 bits from the kernel. `/dev/urandom` rather than a crate: this is the
/// only randomness in the binary and it is an idempotency key, not a secret.
fn randomness() -> [u8; 10] {
    use std::io::Read as _;
    let mut bytes = [0u8; 10];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(&mut bytes);
    }
    bytes
}

/// The report body. `source` is deliberately absent: fleetwatch stamps it from
/// the ingest token, so a producer can only ever write as itself.
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

/// The ingest token, from the environment or the file the fleet's `secret.sh`
/// writes. `None` if there is none — this machine is simply not a producer yet,
/// which is a thing to say plainly, not to crash on.
/// `from_env` is passed in rather than read here: which of the two sources wins
/// is a decision worth testing, and setting an environment variable is `unsafe`
/// in this edition — a test that mutated process-global state under a threaded
/// runner would be the wrong way to reach it.
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
///
/// Fails on a transport error — the caller decides whether a fleetwatch it
/// cannot reach is worth failing over. It is not: the recording is what
/// matters, and an unreachable monitor already shows itself as stale at the
/// other end.
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
