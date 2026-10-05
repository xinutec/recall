//! The browsing read routes over the meaning plane, on read-only connections.
//! The exported structs generate the Angular app's types.

use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

use crate::turn_store::{HiddenKind, Provenance, Stage};

/// Current turns with their source. LEFT JOIN: a correction can exist with no
/// audio segment.
macro_rules! current {
    () => {
        "SELECT t.*, a.source_id FROM transcript_segments t \
         LEFT JOIN audio_segments a ON t.audio_segment_id = a.id \
         WHERE t.superseded_by IS NULL AND (?1 OR t.hidden_reason IS NULL) \
           AND t.start_utc < COALESCE(?3, '9999') AND t.start_utc > COALESCE(?4, '')"
    };
}

macro_rules! ties {
    () => {
        " AND t.start_utc = ?5 AND t.id NOT IN (SELECT value FROM json_each(?6))"
    };
}

crate::statements! {
    SEARCH: Meaning =
        "SELECT ts.*, a.source_id FROM transcript_segments ts \
         JOIN transcript_fts ON transcript_fts.rowid = ts.id \
         LEFT JOIN audio_segments a ON a.id = ts.audio_segment_id \
         WHERE transcript_fts MATCH ?1 AND ts.superseded_by IS NULL \
           AND ts.hidden_reason IS NULL \
         ORDER BY ts.start_utc \
         LIMIT ?2";
    SUPERSEDED_BY: Meaning =
        "SELECT superseded_by FROM transcript_segments WHERE id = ?1";
    BY_ID: Meaning =
        "SELECT t.*, a.source_id FROM transcript_segments t \
         LEFT JOIN audio_segments a ON a.id = t.audio_segment_id WHERE t.id = ?1";
    REVIEW: Meaning =
        "SELECT t.*, a.source_id FROM transcript_segments t \
         LEFT JOIN audio_segments a ON a.id = t.audio_segment_id \
         WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL \
           AND (t.asr_confidence IS NULL OR t.asr_confidence < ?1) \
         ORDER BY t.asr_confidence IS NOT NULL, t.asr_confidence ASC, t.start_utc \
         LIMIT ?2";

    // ?1 shows hidden turns too; ?3/?4 bound the start (NULL: open). The
    // source filter ?2 has its own queries: as a NULL-able test it stops
    // SQLite reaching a session's turns through its audio (6x slower).
    PAGE_NEWEST: Meaning =
        current!(), " ORDER BY t.start_utc DESC, t.id DESC LIMIT ?5";
    PAGE_OLDEST: Meaning =
        current!(), " ORDER BY t.start_utc ASC, t.id ASC LIMIT ?5";
    SOURCE_PAGE_NEWEST: Meaning =
        current!(), " AND a.source_id = ?2 ORDER BY t.start_utc DESC, t.id DESC LIMIT ?5";
    SOURCE_PAGE_OLDEST: Meaning =
        current!(), " AND a.source_id = ?2 ORDER BY t.start_utc ASC, t.id ASC LIMIT ?5";
    // The rest of a page's last instant: ?5 the instant, ?6 the ids already
    // on the page as a JSON array.
    TIES_NEWEST: Meaning =
        current!(), ties!(), " ORDER BY t.id DESC";
    TIES_OLDEST: Meaning =
        current!(), ties!(), " ORDER BY t.id ASC";
    SOURCE_TIES_NEWEST: Meaning =
        current!(), " AND a.source_id = ?2", ties!(), " ORDER BY t.id DESC";
    SOURCE_TIES_OLDEST: Meaning =
        current!(), " AND a.source_id = ?2", ties!(), " ORDER BY t.id ASC";
}

/// One turn as the app shows it. Absent values serialise as `null`: a null
/// confidence means a person confirmed it, a number is a guess's strength.
#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "Transcript")]
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
    pub tier: Stage,
    pub hidden: Option<String>,
    /// The kind of hide; the app checks this, never `hidden`'s spelling.
    pub hidden_as: Option<HiddenKind>,
    pub audio_url: String,
    pub source: Option<String>,
    pub cluster: Option<String>,
    /// A person typed or vouched for these words.
    pub words_checked: bool,
}

#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "TranscriptList")]
pub struct ItemsOut {
    pub items: Vec<TranscriptOut>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PageOut {
    pub items: Vec<TranscriptOut>,
    pub has_more: bool,
}

/// A turn's stored columns, before the display rules.
pub struct Segment {
    pub id: i64,
    pub start_utc: String,
    pub end_utc: String,
    pub text: String,
    pub language: Option<String>,
    pub asr_confidence: Option<f64>,
    pub loudness: Option<f64>,
    pub asr_model: Option<String>,
    pub speaker_label: Option<String>,
    pub speaker_guess: Option<String>,
    pub speaker_score: Option<f64>,
    pub speaker_cluster: Option<String>,
    pub provenance: Option<String>,
    pub hidden_reason: Option<String>,
    pub source_id: Option<String>,
    pub audio_segment_id: Option<i64>,
    pub words_checked: bool,
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
            audio_segment_id: row.get("audio_segment_id")?,
            words_checked: row.get::<_, Option<i64>>("words_checked")? == Some(1),
        })
    }

    /// How much processing this turn has had. An unknown provenance is logged
    /// and read as none.
    fn tier(&self) -> Stage {
        let provenance = self.provenance.as_deref().and_then(|raw| {
            raw.parse::<Provenance>()
                .inspect_err(|err| tracing::warn!(id = self.id, %err, "turn provenance"))
                .ok()
        });
        Stage::of(
            self.asr_model.as_deref(),
            provenance.as_ref(),
            self.speaker_cluster.is_some(),
        )
    }
}

fn to_out(segment: &Segment) -> TranscriptOut {
    to_out_with(segment, None)
}

/// The display rules: a person's label wins and carries no score; otherwise
/// the guess with its strength ("Alice 31%", not "unknown"). `guess` overrides
/// the turn's own for a folded moment, whose strongest match may sit on
/// another mic's version; a person's label still wins.
pub fn to_out_with(
    segment: &Segment,
    guess: Option<(Option<String>, Option<f64>)>,
) -> TranscriptOut {
    let confirmed = segment.speaker_label.is_some();
    let (speaker, speaker_confidence) = if confirmed {
        (segment.speaker_label.clone(), None)
    } else if let Some((name, score)) = guess {
        (name, score)
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
        hidden_as: segment.hidden_reason.as_deref().map(HiddenKind::of),
        audio_url: format!("/api/audio/{}", segment.id),
        source: segment.source_id.clone(),
        cluster: segment.speaker_cluster.clone(),
        words_checked: segment.words_checked,
    }
}

/// Times go out exactly as stored: reformatting could only change them (a `Z`
/// for `+00:00`, dropped microseconds).
pub fn iso(stored: &str) -> String {
    stored.to_owned()
}

/// Open `recall.sqlite` read-only.
pub fn open(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        root.join("recall.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

/// Full-text search over current turns, oldest-first.
pub fn search(conn: &Connection, query: &str, limit: i64) -> rusqlite::Result<ItemsOut> {
    let mut stmt = SEARCH.prepare(conn)?;
    let rows = stmt.query_map((query, limit), Segment::from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(to_out(&row?));
    }
    Ok(ItemsOut { items })
}

/// The current version of a turn, following the supersede chain, so a deep
/// link shows what is true now. Guarded against cycles: several passes write
/// `superseded_by`.
pub fn current_version(conn: &Connection, id: i64) -> rusqlite::Result<Option<TranscriptOut>> {
    let mut seen = std::collections::HashSet::new();
    let mut at = id;
    loop {
        if !seen.insert(at) {
            return Ok(None);
        }
        let next: Option<Option<i64>> = SUPERSEDED_BY
            .query_row(conn, [at], |r| r.get(0))
            .optional()?;
        match next {
            None => return Ok(None), // no such turn
            Some(None) => break,     // `at` is the live one
            Some(Some(newer)) => at = newer,
        }
    }
    let mut stmt = BY_ID.prepare(conn)?;
    let mut rows = stmt.query([at])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(to_out(&Segment::from_row(row)?)))
}

/// Turns by id, resolved to their current versions, in the order asked and
/// deduplicated (several ids can resolve to one turn).
pub fn transcripts(conn: &Connection, ids: &[i64]) -> rusqlite::Result<ItemsOut> {
    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if let Some(out) = current_version(conn, *id)?
            && seen.insert(out.id)
        {
            items.push(out);
        }
    }
    Ok(ItemsOut { items })
}

/// The review queue, least confident first; unscored first of all.
pub fn review(conn: &Connection, max_confidence: f64, limit: i64) -> rusqlite::Result<ItemsOut> {
    let mut stmt = REVIEW.prepare(conn)?;
    let rows = stmt.query_map((max_confidence, limit), Segment::from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(to_out(&row?));
    }
    Ok(ItemsOut { items })
}

/// Which slice of the stream to read. `before` takes the newest page older than
/// the cursor, newest first; `after` the oldest page newer, oldest first, so a
/// forward page joins what the caller holds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Window<'a> {
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub source: Option<&'a str>,
    /// Hidden turns too, with their reason, so a hide can be taken back.
    pub hidden: bool,
}

/// Current turns for one page, plus the rest of the last instant's ties: the
/// cursor is a bare start time that turns often share, so a page can exceed
/// `limit`. Callers test `len >= limit` for more.
pub fn recent(conn: &Connection, limit: i64, window: Window) -> rusqlite::Result<Vec<Segment>> {
    let (page, ties) = match (window.source.is_some(), window.after.is_some()) {
        (false, false) => (PAGE_NEWEST, TIES_NEWEST),
        (false, true) => (PAGE_OLDEST, TIES_OLDEST),
        (true, false) => (SOURCE_PAGE_NEWEST, SOURCE_TIES_NEWEST),
        (true, true) => (SOURCE_PAGE_OLDEST, SOURCE_TIES_OLDEST),
    };
    let filters = (window.hidden, window.source, window.before, window.after);
    let mut segments: Vec<Segment> = page
        .prepare(conn)?
        .query_map(
            rusqlite::params![filters.0, filters.1, filters.2, filters.3, limit],
            Segment::from_row,
        )?
        .collect::<rusqlite::Result<_>>()?;

    let full_page = !segments.is_empty() && i64::try_from(segments.len()).is_ok_and(|n| n == limit);
    if full_page {
        let boundary = segments
            .last()
            .map(|s| s.start_utc.clone())
            .unwrap_or_default();
        let seen: Vec<i64> = segments
            .iter()
            .filter(|s| s.start_utc == boundary)
            .map(|s| s.id)
            .collect();
        let seen = serde_json::to_string(&seen)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
        let mut stmt = ties.prepare(conn)?;
        for tie in stmt.query_map(
            rusqlite::params![filters.0, filters.1, filters.2, filters.3, boundary, seen],
            Segment::from_row,
        )? {
            segments.push(tie?);
        }
    }
    Ok(segments)
}

/// One page of the timeline older than `before` (or the newest page), in
/// conversation order.
pub fn timeline(conn: &Connection, limit: i64, before: Option<&str>) -> rusqlite::Result<PageOut> {
    let mut segments = recent(
        conn,
        limit,
        Window {
            before,
            ..Window::default()
        },
    )?;
    let has_more = i64::try_from(segments.len()).is_ok_and(|n| n >= limit);
    segments.reverse();
    Ok(PageOut {
        items: segments.iter().map(to_out).collect(),
        has_more,
    })
}

// --- the HTTP surface ----------------------------------------------------------

use crate::route;
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

/// So `?limit=10000000` cannot dump the archive.
fn clamp(limit: i64) -> i64 {
    limit.clamp(0, 1000)
}

pub async fn timeline_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<TimelineQuery>,
) -> Response {
    let root = st.root.clone();
    route::json("timeline", move || {
        timeline(&open(&root)?, clamp(q.limit), q.before.as_deref())
    })
    .await
}

#[derive(Deserialize)]
pub struct TranscriptsQuery {
    ids: String,
}

#[derive(Deserialize)]
pub struct ReviewQuery {
    #[serde(default = "default_review_limit")]
    limit: i64,
}

const fn default_review_limit() -> i64 {
    50
}

const REVIEW_MAX_CONFIDENCE: f64 = 0.9;

pub async fn transcripts_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<TranscriptsQuery>,
) -> Response {
    // A bad id is a 400: dropping it would read as "that turn is gone".
    let mut ids = Vec::new();
    for piece in q.ids.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match piece.parse::<i64>() {
            Ok(id) => ids.push(id),
            Err(_) => return (StatusCode::BAD_REQUEST, "ids must be integers").into_response(),
        }
    }
    let root = st.root.clone();
    route::json("transcripts", move || transcripts(&open(&root)?, &ids)).await
}

pub async fn review_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<ReviewQuery>,
) -> Response {
    let root = st.root.clone();
    let limit = clamp(q.limit);
    route::json("review", move || {
        review(&open(&root)?, REVIEW_MAX_CONFIDENCE, limit)
    })
    .await
}

pub async fn search_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<SearchQuery>,
) -> Response {
    let root = st.root.clone();
    route::json("search", move || {
        search(&open(&root)?, &q.q, clamp(q.limit))
    })
    .await
}
