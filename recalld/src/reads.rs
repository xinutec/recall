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

use rusqlite::{Connection, OptionalExtension, Row};
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

/// The live version of a turn, following the supersede chain.
///
/// ⚠ **A deep link points at the id it was made from, which may since have been
/// corrected or reprocessed.** Resolving to the current version is what makes an
/// old link show the text that is true now rather than the text that was true
/// when somebody copied the URL.
///
/// ⚠ **The `seen` set is a CYCLE GUARD, not tidiness.** `superseded_by` is
/// written by several passes; one bad chain would spin this loop forever on a
/// request thread, which is a hang rather than an error.
pub fn current_version(conn: &Connection, id: i64) -> rusqlite::Result<Option<TranscriptOut>> {
    // The chain is walked with a scalar query rather than by widening `Segment`:
    // every other query here filters `superseded_by IS NULL`, so carrying the
    // column on the shared row type would add a field that is always NULL
    // everywhere else it is used.
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
/// ⚠ Deduped: several requested ids can resolve to the SAME live turn once one
/// superseded another, and showing it twice would read as two separate things
/// having been said.
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
/// ⚠ **NULL confidence sorts FIRST** — unknown is the most suspect, not the least.
/// Sorting it last (which is what a plain `ORDER BY` does in some engines) would
/// bury exactly the turns nobody has ever scored.
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

/// One page of the timeline, older than `before` (or the newest page).
///
/// ⚠ **A full page is EXTENDED past `limit` to include every turn tied with its
/// boundary instant.** Turns genuinely share a start time — co-located mics
/// recording the same speech, and corrections — so a page that cut a tie group in
/// half would make the next strict-`<` page skip the group's remainder silently.
/// That is why `hasMore` is `len >= limit` and not `len == limit`.
/// Which slice of the stream to read, mirroring `store.recent_transcripts`.
///
/// ⚠ `before` and `after` are not symmetric. `before` takes the newest page
/// OLDER than the cursor and reads newest-first; `after` takes the oldest page
/// NEWER than it and reads oldest-first, so a forward page is contiguous with
/// what the caller already holds rather than a jump.
#[derive(Debug, Default, Clone, Copy)]
pub struct Window<'a> {
    pub before: Option<&'a str>,
    pub after: Option<&'a str>,
    pub source: Option<&'a str>,
}

/// Current, visible turns for one page, including the boundary instant's ties.
///
/// ⚠ **A full page extends PAST `limit`, on purpose.** The cursor on the wire is
/// a bare start time and turns share one constantly — co-located mics, and
/// corrections that inherit their turn's span. A page cut mid-group would make
/// the next strict-`<` page skip the group's remainder silently, so the boundary
/// instant is completed before returning. That is why callers test
/// `len >= limit` for has-more rather than `==`.
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

    // `!is_empty()` mirrors Python's `if rows and ...`: at limit 0 an empty page
    // must not trigger a tie pass with no boundary.
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

/// A limit is clamped rather than trusted.
///
/// ⚠ The Python takes it straight from the query string, so `?limit=10000000`
/// asks `SQLite` for the whole archive in one page. That is not a hole worth
/// copying: a browsing route that a signed-in person can accidentally turn into
/// an archive dump will eventually be turned into one.
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
    // ⚠ A non-integer id is a 400, never a silently dropped one: the caller asked
    // for a specific set of fragments, and quietly returning fewer would read as
    // "those turns are gone".
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
