//! The browsing tier's read routes over `recall.sqlite` (the meaning plane, not
//! `ingest.sqlite`; see docs/architecture.md). Nothing here writes: every
//! connection is opened read-only.
//!
//! The JSON is a contract with the Angular app, so field names and null-versus-
//! absent are fixed; the exported structs generate its typed contract.

use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

use crate::turn_store::{Provenance, Stage};

/// One turn, in exactly the shape the Angular app already consumes.
///
/// `camelCase` and the explicit `Option`s are the contract: the UI distinguishes
/// a null confidence ("confirmed by a human, no score applies") from a numeric
/// one ("a guess, this strong"), so these serialise as `null` rather than being
/// skipped.
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
    pub audio_url: String,
    pub source: Option<String>,
    pub cluster: Option<String>,
    /// A person typed or vouched for these words, wherever they did it.
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

/// The raw columns a turn is built from, before the display rules are applied.
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
            words_checked: row.get::<_, Option<i64>>("words_checked")? == Some(1),
        })
    }

    /// How much processing this turn has had. A provenance no writer produces is
    /// logged and read as none: the badge is display, and the audit reports it.
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

/// Apply the display rules: a human label is authoritative and carries no score;
/// otherwise show the best auto guess WITH its strength, so the UI can render
/// "Alice 31%" rather than hiding a weak-but-useful guess as "unknown".
fn to_out(segment: &Segment) -> TranscriptOut {
    to_out_with(segment, None)
}

/// The turn as the app consumes it, optionally overriding the auto guess.
///
/// ⚠ The override exists for folded moments only. A spine is chosen for the
/// cleanest TRANSCRIPTION, which says nothing about attribution — the strongest
/// voiceprint match for the same words may sit on another mic's version. A human
/// label still wins over both: `confirmed` is checked first, so an override can
/// never overwrite a name a person gave.
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
        audio_url: format!("/api/audio/{}", segment.id),
        source: segment.source_id.clone(),
        cluster: segment.speaker_cluster.clone(),
        words_checked: segment.words_checked,
    }
}

/// Times go out exactly as stored. The stored text is already isoformat, and
/// re-formatting could only introduce a difference (a `Z` for `+00:00`, or
/// dropped microseconds).
pub fn iso(stored: &str) -> String {
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

/// Open `recall.sqlite` read-only, so a read route cannot write the record.
pub fn open(root: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        root.join("recall.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    // Background passes write this database; a reader waits for them rather
    // than failing.
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

/// The live version of a turn, following the supersede chain.
///
/// A deep link names the id it was made from, which may since have been
/// corrected or reprocessed; resolving it shows the text that is true now.
///
/// `seen` is a cycle guard: several passes write `superseded_by`, and one bad
/// chain would otherwise hang a request thread.
pub fn current_version(conn: &Connection, id: i64) -> rusqlite::Result<Option<TranscriptOut>> {
    // A scalar query rather than a wider `Segment`: every other query filters
    // `superseded_by IS NULL`, so the column would always be NULL there.
    let mut seen = std::collections::HashSet::new();
    let mut at = id;
    loop {
        if !seen.insert(at) {
            return Ok(None); // a cycle: report absent rather than spin
        }
        let next: Option<Option<i64>> = conn
            .query_row(
                "SELECT superseded_by FROM transcript_segments WHERE id = ?1",
                [at],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            None => return Ok(None), // no such turn
            Some(None) => break,     // `at` is the live one
            Some(Some(newer)) => at = newer,
        }
    }
    let mut stmt = conn.prepare(
        "SELECT t.*, a.source_id FROM transcript_segments t \
         LEFT JOIN audio_segments a ON a.id = t.audio_segment_id WHERE t.id = ?1",
    )?;
    let mut rows = stmt.query([at])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(to_out(&Segment::from_row(row)?)))
}

/// Specific turns by id, resolved to their live versions, in the order asked.
///
/// Deduped: several ids can resolve to the same live turn, and showing it twice
/// would read as two things said.
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

/// The review queue: current turns most in need of a human, least confident first.
///
/// NULL confidence sorts first: a turn nobody has scored is the most suspect.
pub fn review(conn: &Connection, max_confidence: f64, limit: i64) -> rusqlite::Result<ItemsOut> {
    let mut stmt = conn.prepare(
        "SELECT t.*, a.source_id FROM transcript_segments t \
         LEFT JOIN audio_segments a ON a.id = t.audio_segment_id \
         WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL \
           AND (t.asr_confidence IS NULL OR t.asr_confidence < ?1) \
         ORDER BY t.asr_confidence IS NOT NULL, t.asr_confidence ASC, t.start_utc \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map((max_confidence, limit), Segment::from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(to_out(&row?));
    }
    Ok(ItemsOut { items })
}

/// Which slice of the stream to read.
///
/// `before` and `after` are not symmetric: `before` takes the newest page older
/// than the cursor, newest-first; `after` takes the oldest page newer than it,
/// oldest-first, so a forward page is contiguous with what the caller holds.
#[derive(Debug, Default, Clone, Copy)]
pub struct Window<'a> {
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub source: Option<&'a str>,
}

/// Current, visible turns for one page, including the boundary instant's ties.
///
/// ⚠ A full page extends past `limit`. The cursor is a bare start time and turns
/// share one often (co-located mics, corrections), so a page cut mid-group would
/// make the next strict-`<` page skip the rest of it. Callers therefore test
/// `len >= limit` for has-more, not `==`.
pub fn recent(conn: &Connection, limit: i64, window: Window) -> rusqlite::Result<Vec<Segment>> {
    let mut filters = String::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(source) = window.source {
        filters.push_str(" AND a.source_id = ?");
        params.push(Box::new(source.to_owned()));
    }
    if let Some(before) = window.before {
        filters.push_str(" AND t.start_utc < ?");
        params.push(Box::new(before.to_owned()));
    }
    if let Some(after) = window.after {
        filters.push_str(" AND t.start_utc > ?");
        params.push(Box::new(after.to_owned()));
    }
    // Forward paging reads oldest-first; every other case newest-first. The id
    // tiebreak makes same-instant order deterministic, and matches the tie pass.
    let order = if window.after.is_some() {
        "ASC"
    } else {
        "DESC"
    };

    let sql =
        format!("{SELECT_VISIBLE}{filters} ORDER BY t.start_utc {order}, t.id {order} LIMIT ?");
    let mut page_params = borrowed(&params);
    let limit_box: Box<dyn rusqlite::ToSql> = Box::new(limit);
    page_params.push(limit_box.as_ref());
    let mut stmt = conn.prepare(&sql)?;
    let mut segments: Vec<Segment> = stmt
        .query_map(page_params.as_slice(), Segment::from_row)?
        .collect::<rusqlite::Result<_>>()?;

    // At limit 0 an empty page must not trigger a tie pass with no boundary.
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
        let marks = vec!["?"; seen.len()].join(",");
        let tie_sql = format!(
            "{SELECT_VISIBLE}{filters} AND t.start_utc = ? AND t.id NOT IN ({marks}) \
             ORDER BY t.id {order}"
        );
        let mut tie_params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        tie_params.push(Box::new(boundary));
        for id in &seen {
            tie_params.push(Box::new(*id));
        }
        let mut refs = borrowed(&params);
        refs.extend(tie_params.iter().map(std::convert::AsRef::as_ref));
        let mut tie_stmt = conn.prepare(&tie_sql)?;
        for tie in tie_stmt.query_map(refs.as_slice(), Segment::from_row)? {
            segments.push(tie?);
        }
    }
    Ok(segments)
}

fn borrowed(params: &[Box<dyn rusqlite::ToSql>]) -> Vec<&dyn rusqlite::ToSql> {
    params.iter().map(std::convert::AsRef::as_ref).collect()
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
    // Newest-first from the DB; reverse so the page reads top-to-bottom in
    // conversation order.
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

/// A limit is clamped rather than trusted, so `?limit=10000000` cannot turn a
/// browsing route into an archive dump.
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

/// Turns most in need of a human are those the model was least sure of.
const REVIEW_MAX_CONFIDENCE: f64 = 0.9;

pub async fn transcripts_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    Query(q): Query<TranscriptsQuery>,
) -> Response {
    // A non-integer id is a 400, not silently dropped: returning fewer turns
    // than asked would read as "those turns are gone".
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
