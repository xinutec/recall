//! Per-source liveness: which recorders are measurably recording right now.
//!
//! Every source reduces to a last-proved-recording time from its liveness
//! marker — refreshed by the ingest pump while a phone streams real signal, and
//! by the capture watchdog while the local mic's closed segments decode to real
//! audio. **"Active" therefore means RECORDING, never merely connected**: a
//! phone streaming digital silence, or a mic in a startup dead-window, reads
//! idle.
//!
//! ⚠ **The fleet has no markers of its own.** It runs no capture and no ingest
//! pump, so every time here arrives via the Mac's ~5 s mirror report and is one
//! report-cadence old. That lag is why the windows widen on this side, and why a
//! Mac that stops reporting must read as "we cannot see" rather than as the last
//! thing we heard.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use std::collections::HashMap;

/// How a source's PCM stream is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    CoreAudio,
    Lavfi,
    Rtsp,
    TcpPcm,
    Upload,
    Discovered,
    /// A stream this system BUILT rather than recorded: the room stream, one
    /// settled minute at a time from whichever microphone won it (stage D3).
    ///
    /// Not a device, and the distinction is load-bearing: `deaf`, the liveness
    /// view and the sources panel all ask `is_device()`, and a derived stream
    /// has no recorder to be deaf, no `.alive` marker, and no phone to blame.
    /// It inherits whichever microphone's audio it carried, so measuring it as a
    /// microphone would double-count the one that was already measured.
    Derived,
}

impl SourceKind {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "coreaudio" => Self::CoreAudio,
            "lavfi" => Self::Lavfi,
            "rtsp" => Self::Rtsp,
            "tcp_pcm" => Self::TcpPcm,
            "upload" => Self::Upload,
            "discovered" => Self::Discovered,
            "derived" => Self::Derived,
            _ => return None,
        })
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CoreAudio => "coreaudio",
            Self::Lavfi => "lavfi",
            Self::Rtsp => "rtsp",
            Self::TcpPcm => "tcp_pcm",
            Self::Upload => "upload",
            Self::Discovered => "discovered",
            Self::Derived => "derived",
        }
    }

    /// Is this a recorder whose up-or-down state is a real question?
    ///
    /// ⚠ `Upload` is a clip someone sent, not a producer. `Discovered` is audio
    /// the worker found on disk with no registered source — an admission that
    /// nothing knows what wrote it, so every device check would be asking about
    /// a machine that may not exist. If a discovered source really is a
    /// recorder, its agent registers the true kind on start and it joins this
    /// set then.
    #[must_use]
    pub fn is_device(self) -> bool {
        !matches!(self, Self::Upload | Self::Discovered | Self::Derived)
    }
}

/// A registered source, as the liveness view needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
}

/// What a recorder's DELIVERIES prove — two times, because they answer two
/// different questions and collapsing them is the #1428 bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Evidence {
    /// Newest segment sent, whatever was on it: "is it running".
    pub delivered: Option<DateTime<Utc>>,
    /// Newest segment that could be someone talking: "is my voice being
    /// captured audibly". `None` when nothing audible has been measured.
    pub speech: Option<DateTime<Utc>>,
}

/// One recorder's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    pub source_id: String,
    pub name: String,
    pub kind: SourceKind,
    pub last_active: Option<DateTime<Utc>>,
    /// The CONSENT signal — "your voice is being captured audibly" — so it goes
    /// out in a silent room.
    pub active: bool,
    /// The OPERATIONAL one — bytes arriving, whatever is on them. Answering the
    /// second with the first is how geb came to read "off" while recording
    /// perfectly (#1428).
    pub recording: bool,
    pub last_delivered: Option<DateTime<Utc>>,
}

/// A streamed phone's marker is refreshed sub-second while signal flows.
fn active_within() -> Duration {
    Duration::seconds(5)
}
/// The mic's marker is refreshed by the dead-segment watchdog each healthy poll
/// (every 30 s): two missable polls plus margin.
fn watchdog_active_within() -> Duration {
    Duration::seconds(75)
}
/// On the fleet the markers arrive via the Mac's ~5 s mirror report, not a local
/// file, so every time reads a report-cadence older here.
fn fleet_report_lag() -> Duration {
    Duration::seconds(7)
}
/// A store-and-forward recorder streams to nothing, so no marker of its is ever
/// refreshed; it proves itself by DELIVERING a closed segment. That evidence
/// arrives once per segment: 60 s of audio must close, then wait up to the 60 s
/// upload timer, then transfer and verify. Five minutes covers it with margin —
/// wide enough not to flap, narrow enough that a dead recorder does not read
/// live for long.
fn delivered_active_within() -> Duration {
    Duration::minutes(5)
}

/// How fresh `kind`'s marker must be to call the source recording.
#[must_use]
pub fn active_window(kind: SourceKind, on_fleet: bool) -> Duration {
    let base = if kind == SourceKind::TcpPcm {
        active_within()
    } else {
        watchdog_active_within()
    };
    base + if on_fleet {
        fleet_report_lag()
    } else {
        Duration::zero()
    }
}

/// Combine registered sources with their last-activity time.
///
/// ⚠ Both times must be CAPTURE times, never arrival times: a backlog draining
/// hours late arrives now and proves nothing about now.
#[must_use]
pub fn source_statuses<S: std::hash::BuildHasher, T: std::hash::BuildHasher>(
    sources: &[SourceRow],
    last_active: &HashMap<String, DateTime<Utc>, S>,
    now: DateTime<Utc>,
    on_fleet: bool,
    delivered: &HashMap<String, Evidence, T>,
) -> Vec<SourceStatus> {
    sources
        .iter()
        .map(|row| {
            let seen = last_active.get(&row.id).copied();
            let window = active_window(row.kind, on_fleet);
            let marker_fresh = seen.is_some_and(|t| now - t < window);
            // ⚠ A marker that went stale RECENTLY is a deliberate stop, and that
            // is NEWER information than a segment captured just before it.
            // Without this, delivery-proof resurrects a phone the moment its
            // owner stops it: measured 2026-09-05, pixel9 stayed green for the
            // full five minutes after stopping, where it used to go idle in
            // twelve seconds. A phone streams as its PRIMARY path, so its marker
            // falling silent IS the event; geb's marker is hours stale only
            // because it never streams at all. How stale is what tells them
            // apart.
            let stopped_recently =
                seen.is_some_and(|t| !marker_fresh && now - t < delivered_active_within());
            let fresh = |when: Option<DateTime<Utc>>| {
                !stopped_recently && when.is_some_and(|t| now - t < delivered_active_within())
            };

            let evidence = delivered.get(&row.id).copied().unwrap_or_default();
            let shipped = evidence.delivered;
            let heard = evidence.speech;
            SourceStatus {
                source_id: row.id.clone(),
                name: row.name.clone(),
                kind: row.kind,
                last_active: [seen, heard].into_iter().flatten().max(),
                // The marker is itself signal-gated (refreshed only above the
                // silence floor), so a fresh marker IS evidence of audible
                // speech.
                active: marker_fresh || fresh(heard),
                recording: marker_fresh || fresh(shipped),
                last_delivered: [seen, shipped].into_iter().flatten().max(),
            }
        })
        .collect()
}

/// Registered sources, for the liveness view.
///
/// ⚠ An unknown kind in the database fails LOUD here rather than becoming a
/// silently never-matching string downstream — the same choice the Python's
/// `SourceKind(...)` makes by raising.
pub fn source_rows(conn: &Connection) -> rusqlite::Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare("SELECT id, name, kind FROM sources ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        let raw: String = r.get(2)?;
        let kind = SourceKind::parse(&raw).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                format!("unknown source kind {raw:?}").into(),
            )
        })?;
        Ok(SourceRow {
            id: r.get(0)?,
            name: r.get(1)?,
            kind,
        })
    })?;
    rows.collect()
}

/// The delivered-segment evidence, read straight from the ingest database.
///
/// ⚠ The Python fetched this over HTTP from recalld's own `/ingest/v1/liveness`
/// — a loopback request, with a token and a 1.5 s timeout, made from inside a UI
/// poll. On this side it is a query, so the hop, the credential and the timeout
/// all disappear. What must NOT disappear is its best-effort contract: an
/// unreadable ingest database means "no extra evidence", never an error page,
/// because the panel is still correct on markers alone.
fn delivered_evidence(root: &std::path::Path) -> HashMap<String, Evidence> {
    let Ok(conn) = crate::store::open(root) else {
        return HashMap::new();
    };
    let Ok(rows) = crate::speech::liveness_by_source(&conn) else {
        return HashMap::new();
    };
    rows.into_iter()
        .filter_map(|(source, delivered, speech)| {
            // A source with no parseable delivered time carries no evidence at
            // all — the Python drops the entry rather than inventing one.
            let delivered = crate::instant::parse(&delivered)?.with_timezone(&Utc);
            Some((
                source,
                Evidence {
                    delivered: Some(delivered),
                    speech: crate::instant::parse(&speech).map(|t| t.with_timezone(&Utc)),
                },
            ))
        })
        .collect()
}

/// The wire shape of one row of `GET /api/sources`.
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct SourceOut {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub active: bool,
    #[serde(rename = "lastActive")]
    pub last_active: Option<String>,
    /// Separate from `active` on purpose — see [`SourceStatus`].
    pub recording: bool,
    #[serde(rename = "lastDelivered")]
    pub last_delivered: Option<String>,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct SourcesOut {
    pub items: Vec<SourceOut>,
}

/// Everything `GET /api/sources` needs, computed off the request thread.
///
/// ⚠ **Delivery evidence is discarded outright while capture is paused**, and
/// for EVERY kind rather than just the local mic. Delivered segments are up to a
/// segment old, so audio captured in the seconds before a pause would otherwise
/// keep a dot green for the whole delivered window — the exact opposite of the
/// promise a pause makes. A pause stops the phones and the machines too, so none
/// of them may be resurrected by what they recorded just before it.
pub fn fleet_sources(
    root: &std::path::Path,
    conn: &Connection,
    now: DateTime<Utc>,
) -> rusqlite::Result<SourcesOut> {
    let rows: Vec<SourceRow> = source_rows(conn)?
        .into_iter()
        .filter(|r| r.kind.is_device())
        .collect();
    let running = crate::capture::fleet_capture_state(conn, now)?.running;
    let reported = crate::capture::reported_source_liveness(conn, now)?.unwrap_or_default();

    let mut last_active: HashMap<String, DateTime<Utc>> = HashMap::new();
    for row in &rows {
        // The mic keeps a pause gate the streamed phones do not need: its
        // window is a leisurely ~75 s (the watchdog's cadence), and a pause must
        // read idle at once rather than after one more poll.
        if let Some(when) = reported
            .get(&row.id)
            .filter(|_| row.kind == SourceKind::TcpPcm || running)
        {
            last_active.insert(row.id.clone(), *when);
        }
    }

    let known: std::collections::HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let delivered: HashMap<String, Evidence> = if running {
        delivered_evidence(root)
            .into_iter()
            .filter(|(source, _)| known.contains(source.as_str()))
            .collect()
    } else {
        HashMap::new()
    };

    Ok(SourcesOut {
        items: source_statuses(&rows, &last_active, now, true, &delivered)
            .into_iter()
            .map(|s| SourceOut {
                id: s.source_id,
                name: s.name,
                kind: s.kind.as_str(),
                active: s.active,
                last_active: s.last_active.map(crate::instant::python_isoformat_utc),
                recording: s.recording,
                last_delivered: s.last_delivered.map(crate::instant::python_isoformat_utc),
            })
            .collect(),
    })
}

/// `GET /api/sources` — per-recorder liveness for the fleet view.
///
/// Uploaded recordings are sources but not live devices, so they are excluded;
/// they live in the Sessions view.
pub async fn sources_route(
    axum::extract::State(st): axum::extract::State<std::sync::Arc<crate::reads::State>>,
) -> axum::response::Response {
    let root = st.root.clone();
    crate::route::json("sources", move || {
        fleet_sources(&root, &crate::work::open_write(&root)?, Utc::now())
    })
    .await
}
