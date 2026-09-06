//! The browsing tier's read routes (stage F1), ported from `recall.api_reads`.
//!
//! This is the first route group to move, and it is deliberately the read-only
//! one: a port that can only ever answer questions cannot destroy anything if it
//! is wrong, and it was checked against the Python it replaces by asking both the
//! SAME question about the SAME database and diffing the JSON — 10 cases over the
//! real archive, byte identical. Nothing here writes.
//!
//! ⚠ **This reads `recall.sqlite`, NOT `ingest.sqlite`.** The audio plane and the
//! meaning plane stay split (docs/audio-plane.md): blobs plus `ingest.sqlite` are
//! recalld's own, while `recall.sqlite` remains the transcript system of record
//! that the Python tier also has open. So every connection here is opened
//! READ-ONLY and in WAL — recalld must not be able to write a plane it does not
//! own, and must never block a writer that does.
//!
//! ⚠ **The JSON is a CONTRACT with a shipped Angular app**, so the field names
//! and the null-vs-absent distinction are copied, not redesigned. Anything that
//! looks like it wants tidying here is load-bearing until F1 regenerates the
//! frontend's typed contract from these structs.

use rusqlite::{Connection, Row};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

/// `asr_model` of a turn a human corrected — the top tier, never re-derived.
const HUMAN_MODEL: &str = "human";
/// `asr_model` of the provisional live pass.
const LIVE_MODEL: &str = "live";
/// `provenance` prefix written by the diarized refine pass.
const DIARIZED_MARKER: &str = "diarized";

/// One turn, in exactly the shape the Angular app already consumes.
///
/// `camelCase` and the explicit `Option`s are the contract: the UI distinguishes
/// a null confidence ("confirmed by a human, no score applies") from a numeric
/// one ("a guess, this strong"), so these serialise as `null` rather than being
/// skipped.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptOut {
    pub id: i64,
    pub start: String,
    pub end: String,
    pub text: String,
    pub language: Option<String>,
    pub speaker: Option<String>,
    pub speaker_confirmed: bool,
    pub speaker_confidence: Option<f64>,
    pub confidence: Option<f64>,
    pub loudness: Option<f64>,
    pub model: Option<String>,
    pub tier: &'static str,
    pub hidden: Option<String>,
    pub audio_url: String,
    pub source: Option<String>,
    pub cluster: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ItemsOut {
    pub items: Vec<TranscriptOut>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PageOut {
    pub items: Vec<TranscriptOut>,
    pub has_more: bool,
}

/// The raw columns a turn is built from, before the display rules are applied.
struct Segment {
    id: i64,
    start_utc: String,
    end_utc: String,
    text: String,
    language: Option<String>,
    asr_confidence: Option<f64>,
    loudness: Option<f64>,
    asr_model: Option<String>,
    speaker_label: Option<String>,
    speaker_guess: Option<String>,
    speaker_score: Option<f64>,
    speaker_cluster: Option<String>,
    provenance: Option<String>,
    hidden_reason: Option<String>,
    source_id: Option<String>,
}

impl Segment {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            start_utc: row.get("start_utc")?,
            end_utc: row.get("end_utc")?,
            text: row.get("text")?,
            language: row.get("language")?,
            asr_confidence: row.get("asr_confidence")?,
            loudness: row.get("loudness")?,
            asr_model: row.get("asr_model")?,
            speaker_label: row.get("speaker_label")?,
            speaker_guess: row.get("speaker_guess")?,
            speaker_score: row.get("speaker_score")?,
            speaker_cluster: row.get("speaker_cluster")?,
            provenance: row.get("provenance")?,
            hidden_reason: row.get("hidden_reason")?,
            source_id: row.get("source_id")?,
        })
    }

    /// Which analysis tier produced this turn — a UI badge showing how much
    /// processing it has had.
    fn tier(&self) -> &'static str {
        if self.asr_model.as_deref() == Some(HUMAN_MODEL) {
            return "corrected";
        }
        if self.asr_model.as_deref() == Some(LIVE_MODEL) {
            return "live";
        }
        if self
            .provenance
            .as_deref()
            .unwrap_or("")
            .starts_with(DIARIZED_MARKER)
        {
            return "diarized";
        }
        "transcribed"
    }
}

/// Apply the display rules: a human label is authoritative and carries no score;
/// otherwise show the best auto guess WITH its strength, so the UI can render
/// "Alice 31%" rather than hiding a weak-but-useful guess as "unknown".
fn to_out(segment: &Segment) -> TranscriptOut {
    let confirmed = segment.speaker_label.is_some();
    let (speaker, speaker_confidence) = if confirmed {
        (segment.speaker_label.clone(), None)
    } else {
        (segment.speaker_guess.clone(), segment.speaker_score)
    };
    TranscriptOut {
        id: segment.id,
        start: iso(&segment.start_utc),
        end: iso(&segment.end_utc),
        text: segment.text.clone(),
        language: segment.language.clone(),
        speaker,
        speaker_confirmed: confirmed,
        speaker_confidence,
        confidence: segment.asr_confidence,
        loudness: segment.loudness,
        model: segment.asr_model.clone(),
        tier: segment.tier(),
        hidden: segment.hidden_reason.clone(),
        audio_url: format!("/api/audio/{}", segment.id),
        source: segment.source_id.clone(),
        cluster: segment.speaker_cluster.clone(),
    }
}

/// Times go out exactly as stored.
///
/// ⚠ This is a PASS-THROUGH on purpose, and it is the one place this port could
/// diverge invisibly. The Python parses the stored string into a `datetime` and
/// re-emits it with `.isoformat()`, so the two agree only while the stored text
/// is already canonical isoformat — which it is, because the same `.isoformat()`
/// wrote it. Re-formatting here would be the way to introduce a difference (a
/// `Z` for a `+00:00`, or dropped microseconds), not to avoid one. The parity
/// test compares these strings against the live Python on the real archive, so a
/// row that ever breaks the assumption shows up as a diff rather than as a
/// subtly wrong timestamp in the UI.
fn iso(stored: &str) -> String {
    stored.to_owned()
}

/// The visible-turn projection both queries share: current (not superseded), not
/// hidden, and carrying the capturing source.
///
/// LEFT JOIN, not JOIN: a correction can exist with no audio segment, and the
/// timeline must still show it.
const SELECT_VISIBLE: &str = "SELECT t.*, a.source_id FROM transcript_segments t \
     LEFT JOIN audio_segments a ON t.audio_segment_id = a.id \
     WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL";

/// Open `recall.sqlite` read-only. See the module note: recalld does not own
/// this plane and must not be able to write it.
pub fn open(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        root.join("recall.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    // The Python tier writes this database; a reader must wait for it rather
    // than fail, and must never hold it up.
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

/// Full-text search over current turns, oldest-first.
pub fn search(conn: &Connection, query: &str, limit: i64) -> rusqlite::Result<ItemsOut> {
    // The FTS join is what makes this a search rather than a scan; ts.* keeps the
    // row shape identical to the timeline's.
    let mut stmt = conn.prepare(
        "SELECT ts.*, a.source_id FROM transcript_segments ts \
         JOIN transcript_fts ON transcript_fts.rowid = ts.id \
         LEFT JOIN audio_segments a ON a.id = ts.audio_segment_id \
         WHERE transcript_fts MATCH ?1 AND ts.superseded_by IS NULL \
           AND ts.hidden_reason IS NULL \
         ORDER BY ts.start_utc \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map((query, limit), Segment::from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(to_out(&row?));
    }
    Ok(ItemsOut { items })
}

/// One page of the timeline, older than `before` (or the newest page).
///
/// ⚠ **A full page is EXTENDED past `limit` to include every turn tied with its
/// boundary instant.** Turns genuinely share a start time — co-located mics
/// recording the same speech, and corrections — so a page that cut a tie group in
/// half would make the next strict-`<` page skip the group's remainder silently.
/// That is why `hasMore` is `len >= limit` and not `len == limit`.
pub fn timeline(conn: &Connection, limit: i64, before: Option<&str>) -> rusqlite::Result<PageOut> {
    let (sql, rows) = match before {
        Some(cursor) => (
            format!(
                "{SELECT_VISIBLE} AND t.start_utc < ?1 ORDER BY t.start_utc DESC, t.id DESC LIMIT ?2"
            ),
            Some(cursor.to_owned()),
        ),
        None => (
            format!("{SELECT_VISIBLE} ORDER BY t.start_utc DESC, t.id DESC LIMIT ?1"),
            None,
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let mut segments: Vec<Segment> = match &rows {
        Some(cursor) => stmt
            .query_map((cursor, limit), Segment::from_row)?
            .collect::<rusqlite::Result<_>>()?,
        None => stmt
            .query_map((limit,), Segment::from_row)?
            .collect::<rusqlite::Result<_>>()?,
    };

    // `!is_empty()` mirrors Python's `if rows and ...`: at limit 0 an empty page
    // must not trigger a tie pass with no boundary.
    let full_page = !segments.is_empty() && i64::try_from(segments.len()).is_ok_and(|n| n == limit);
    if full_page {
        // Pull in the boundary instant's remaining ties. They satisfy the
        // `before` bound by definition, having the boundary's own time.
        let boundary = segments
            .last()
            .map(|s| s.start_utc.clone())
            .unwrap_or_default();
        let seen: Vec<i64> = segments
            .iter()
            .filter(|s| s.start_utc == boundary)
            .map(|s| s.id)
            .collect();
        let marks = vec!["?"; seen.len()].join(",");
        let tie_sql = format!(
            "{SELECT_VISIBLE} AND t.start_utc = ? AND t.id NOT IN ({marks}) ORDER BY t.id DESC"
        );
        let mut tie_stmt = conn.prepare(&tie_sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(boundary)];
        for id in &seen {
            params.push(Box::new(*id));
        }
        let refs: Vec<&dyn rusqlite::ToSql> =
            params.iter().map(std::convert::AsRef::as_ref).collect();
        let ties = tie_stmt.query_map(refs.as_slice(), Segment::from_row)?;
        for tie in ties {
            segments.push(tie?);
        }
    }

    let has_more = i64::try_from(segments.len()).is_ok_and(|n| n >= limit);
    // Newest-first from the DB; reverse so the page reads top-to-bottom in
    // conversation order.
    segments.reverse();
    Ok(PageOut {
        items: segments.iter().map(to_out).collect(),
        has_more,
    })
}

// --- the HTTP surface ----------------------------------------------------------

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;

/// What the read routes need: where `recall.sqlite` lives.
pub struct State {
    pub root: std::path::PathBuf,
}

#[derive(Deserialize)]
pub struct TimelineQuery {
    #[serde(default = "default_timeline_limit")]
    limit: i64,
    before: Option<String>,
}

const fn default_timeline_limit() -> i64 {
    200
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: i64,
}

const fn default_search_limit() -> i64 {
    100
}

/// A limit is clamped rather than trusted.
///
/// ⚠ The Python takes it straight from the query string, so `?limit=10000000`
/// asks `SQLite` for the whole archive in one page. That is not a hole worth
/// copying: a browsing route that a signed-in person can accidentally turn into
/// an archive dump will eventually be turned into one.
fn clamp(limit: i64) -> i64 {
    limit.clamp(0, 1000)
}

fn failed(err: &rusqlite::Error) -> Response {
    tracing::warn!("read query failed: {err}");
    (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response()
}

pub async fn timeline_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<TimelineQuery>,
) -> Response {
    let root = st.root.clone();
    let page = tokio::task::spawn_blocking(move || {
        let conn = open(&root)?;
        timeline(&conn, clamp(q.limit), q.before.as_deref())
    })
    .await;
    match page {
        Ok(Ok(p)) => axum::Json(p).into_response(),
        Ok(Err(e)) => failed(&e),
        Err(e) => {
            tracing::warn!("read task failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response()
        }
    }
}

pub async fn search_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<SearchQuery>,
) -> Response {
    let root = st.root.clone();
    let hits = tokio::task::spawn_blocking(move || {
        let conn = open(&root)?;
        search(&conn, &q.q, clamp(q.limit))
    })
    .await;
    match hits {
        Ok(Ok(h)) => axum::Json(h).into_response(),
        Ok(Err(e)) => failed(&e),
        Err(e) => {
            tracing::warn!("read task failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "read failed").into_response()
        }
    }
}
