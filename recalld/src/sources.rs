//! Which recorders are measurably recording right now.
//!
//! A source's liveness marker is refreshed by the ingest pump while a phone
//! streams real signal, and by the capture watchdog while the mic's segments
//! decode to real audio: a phone streaming silence reads idle. The fleet sees
//! markers only through the Mac's ~5 s report, hence wider windows here.

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use std::collections::HashMap;

crate::statements! {
    SOURCES: Meaning =
        "SELECT id, name, kind FROM sources ORDER BY id";
}

/// How a source's PCM stream is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    CoreAudio,
    Lavfi,
    Rtsp,
    TcpPcm,
    Upload,
    Discovered,
    /// A stream built rather than recorded (the retired room stream). Not a
    /// device: measuring it would double-count the mic whose audio it carries.
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

    /// A recorder whose up-or-down state is a real question. `Discovered` is
    /// audio found on disk with no registered source; a real recorder's agent
    /// registers its true kind on start.
    #[must_use]
    pub fn is_device(self) -> bool {
        !matches!(self, Self::Upload | Self::Discovered | Self::Derived)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
}

/// What a recorder's deliveries prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Evidence {
    /// Newest segment sent: is it running.
    pub delivered: Option<DateTime<Utc>>,
    /// Newest segment with possible speech: is a voice captured audibly.
    pub speech: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    pub source_id: String,
    pub name: String,
    pub kind: SourceKind,
    pub last_active: Option<DateTime<Utc>>,
    /// The consent signal: a voice is being captured audibly. Off in a silent
    /// room.
    pub active: bool,
    /// Bytes arriving, whatever is on them.
    pub recording: bool,
    pub last_delivered: Option<DateTime<Utc>>,
}

/// A streamed phone's marker is refreshed sub-second while signal flows.
fn active_within() -> Duration {
    Duration::seconds(5)
}
/// The watchdog refreshes the mic's marker every 30 s: two missable polls plus
/// margin.
fn watchdog_active_within() -> Duration {
    Duration::seconds(75)
}
/// The Mac's ~5 s report.
fn fleet_report_lag() -> Duration {
    Duration::seconds(7)
}
/// A store-and-forward recorder proves itself by delivering a segment: up to
/// 60 s to close, 60 s to the upload timer, plus transfer.
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

/// Combine registered sources with their last activity. Both times are capture
/// times: a backlog arriving hours late proves nothing about now.
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
            // A marker gone stale recently is a deliberate stop; without this a
            // stopped phone stays green for the delivery window. A recorder that
            // never streams has a marker hours stale.
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
                // The marker is refreshed only above the silence floor.
                active: marker_fresh || fresh(heard),
                recording: marker_fresh || fresh(shipped),
                last_delivered: [seen, shipped].into_iter().flatten().max(),
            }
        })
        .collect()
}

/// Registered sources. An unknown kind is an error, not a silent mismatch.
pub fn source_rows(conn: &Connection) -> rusqlite::Result<Vec<SourceRow>> {
    let mut stmt = SOURCES.prepare(conn)?;
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

/// The delivered-segment evidence. An unreadable ingest plane gives none: the
/// markers alone are still correct.
fn delivered_evidence(root: &std::path::Path) -> HashMap<String, Evidence> {
    let Ok(conn) = crate::store::open(root) else {
        return HashMap::new();
    };
    let Ok(rows) = crate::speech::liveness_by_source(&conn) else {
        return HashMap::new();
    };
    rows.into_iter()
        .filter_map(|(source, delivered, speech)| {
            let delivered = audiocore::instant::parse(&delivered)?.with_timezone(&Utc);
            Some((
                source,
                Evidence {
                    delivered: Some(delivered),
                    speech: audiocore::instant::parse(&speech).map(|t| t.with_timezone(&Utc)),
                },
            ))
        })
        .collect()
}

/// One row of `GET /api/sources`.
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct SourceOut {
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub active: bool,
    #[serde(rename = "lastActive")]
    pub last_active: Option<String>,
    /// See [`SourceStatus`].
    pub recording: bool,
    #[serde(rename = "lastDelivered")]
    pub last_delivered: Option<String>,
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub struct SourcesOut {
    pub items: Vec<SourceOut>,
}

/// `GET /api/sources`'s answer. Delivery evidence is ignored while capture is
/// paused: audio from just before a pause would keep a dot green.
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
        // The mic's ~75 s window would show a pause one poll late; the phones'
        // is short enough without the gate.
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
                last_active: s.last_active.map(audiocore::instant::python_isoformat_utc),
                recording: s.recording,
                last_delivered: s
                    .last_delivered
                    .map(audiocore::instant::python_isoformat_utc),
            })
            .collect(),
    })
}

/// `GET /api/sources`: per-recorder liveness. Uploads are not devices.
pub async fn sources_route(
    axum::extract::State(st): axum::extract::State<std::sync::Arc<crate::reads::State>>,
) -> axum::response::Response {
    let root = st.root.clone();
    crate::route::json("sources", move || {
        fleet_sources(&root, &crate::work::open_write(&root)?, Utc::now())
    })
    .await
}
