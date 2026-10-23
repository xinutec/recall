//! The browsing read routes over the rendered view (`view`), on read-only
//! connections. The exported structs generate the Angular app's types.

use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;
use transcript::{Instant, Name, SourceId, Span};

use crate::line::{HiddenKind, HiddenReason, Stage};
use audiocore::instant::Stamp;

/// The shown lines, and the lines of a stretch a person excluded, which the
/// app folds into one row with its undo; with `?1`, every hidden line too.
/// `'excluded'` is [`HiddenReason::Excluded`]'s phrase.
macro_rules! current {
    () => {
        "SELECT t.* FROM rendered_lines t \
         WHERE (?1 OR t.hidden_reason IS NULL OR t.hidden_reason = 'excluded') \
           AND t.start_utc < COALESCE(?3, '9999') AND t.start_utc > COALESCE(?4, '')"
    };
}

macro_rules! ties {
    () => {
        " AND t.start_utc = ?5 AND t.id NOT IN (SELECT value FROM json_each(?6))"
    };
}

crate::statements! {
    SEES_LINE: Meaning = "SELECT EXISTS (SELECT 1 FROM rendered_lines WHERE id = ?1)";
    SEES_EDIT: Meaning = "SELECT EXISTS (SELECT 1 FROM edits WHERE id = ?1)";
    SEES_SOURCE: Meaning = "SELECT EXISTS (SELECT 1 FROM sources WHERE id = ?1)";
    SEES_DEVICE: Meaning =
        "SELECT EXISTS (SELECT 1 FROM sources WHERE id = ?1)
             OR NOT EXISTS (SELECT 1 FROM main.sources WHERE id = ?1)";
    OWN_SOURCES: Meaning = "SELECT id FROM sources";
    SEES_TERM: Meaning = "SELECT EXISTS (SELECT 1 FROM vocabulary WHERE id = ?1)";
    SEARCH: Meaning =
        "SELECT t.* FROM rendered_lines t \
         JOIN rendered_fts ON rendered_fts.rowid = t.id \
         WHERE rendered_fts MATCH ?1 AND t.hidden_reason IS NULL \
         ORDER BY t.start_utc \
         LIMIT ?2";
    BY_ID: Meaning = "SELECT t.* FROM rendered_lines t WHERE t.id = ?1";
    REVIEW: Meaning =
        "SELECT t.* FROM rendered_lines t \
         WHERE t.hidden_reason IS NULL \
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
        current!(), " AND t.source_id = ?2 ORDER BY t.start_utc DESC, t.id DESC LIMIT ?5";
    SOURCE_PAGE_OLDEST: Meaning =
        current!(), " AND t.source_id = ?2 ORDER BY t.start_utc ASC, t.id ASC LIMIT ?5";
    // The rest of a page's last instant: ?5 the instant, ?6 the ids already
    // on the page as a JSON array.
    TIES_NEWEST: Meaning =
        current!(), ties!(), " ORDER BY t.id DESC";
    TIES_OLDEST: Meaning =
        current!(), ties!(), " ORDER BY t.id ASC";
    SOURCE_TIES_NEWEST: Meaning =
        current!(), " AND t.source_id = ?2", ties!(), " ORDER BY t.id DESC";
    SOURCE_TIES_OLDEST: Meaning =
        current!(), " AND t.source_id = ?2", ties!(), " ORDER BY t.id ASC";
}

/// Who a line is put down to, as the app shows it.
#[derive(Debug, Clone, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "Speaker")]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SpeakerOut {
    /// A name a person gave: nothing to be unsure about.
    Named { name: String },
    /// The voiceprint's best match, and how sure, 0 to 1.
    Guessed { name: String, score: f64 },
}

impl From<&Attribution> for SpeakerOut {
    fn from(who: &Attribution) -> Self {
        match who {
            Attribution::Named(name) => Self::Named {
                name: name.as_str().to_owned(),
            },
            Attribution::Guessed { name, score } => Self::Guessed {
                name: name.as_str().to_owned(),
                score: *score,
            },
        }
    }
}

/// One turn as the app shows it. Absent values serialise as `null`.
#[derive(Debug, Serialize, PartialEq, ts_rs::TS)]
#[ts(export, rename = "Transcript")]
#[serde(rename_all = "camelCase")]
pub struct TranscriptOut {
    pub id: i64,
    pub start: String,
    pub end: String,
    pub text: String,
    pub language: Option<String>,
    pub speaker: Option<SpeakerOut>,
    pub confidence: Option<f64>,
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
    /// The stored spellings, which the wire repeats.
    pub start_utc: Stamp,
    pub end_utc: Stamp,
    /// The same instants, in order: checked once, where the row is read.
    pub span: Span,
    pub text: String,
    pub language: Option<String>,
    pub asr_confidence: Option<f64>,
    pub asr_model: Option<String>,
    pub speaker: Option<Attribution>,
    /// The diarizer's voice label, beside whoever the line is put down to.
    pub speaker_cluster: Option<String>,
    pub tier: Stage,
    pub hidden_reason: Option<HiddenReason>,
    pub source_id: Option<SourceId>,
    pub audio_segment_id: Option<i64>,
    pub words_checked: bool,
    /// Its words with their times; empty for a line nobody timed.
    pub words: Vec<crate::line::WordTiming>,
}

impl Segment {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let start_utc: Stamp = row.get("start_utc")?;
        let end_utc: Stamp = row.get("end_utc")?;
        let span = Span::new(
            Instant::from_utc(start_utc.utc()),
            Instant::from_utc(end_utc.utc()),
        )
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("a line ending before it starts: {start_utc} to {end_utc}").into(),
            )
        })?;
        Ok(Self {
            id: row.get("id")?,
            start_utc,
            end_utc,
            span,
            text: row.get("text")?,
            language: row.get("language")?,
            asr_confidence: row.get("asr_confidence")?,
            asr_model: row.get("asr_model")?,
            speaker: Attribution::read(row)?,
            speaker_cluster: row.get("speaker_cluster")?,
            tier: {
                let stored: String = row.get("tier")?;
                Stage::parse(&stored).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        format!("unknown tier {stored:?}").into(),
                    )
                })?
            },
            hidden_reason: row.get("hidden_reason")?,
            source_id: {
                let index = row.as_ref().column_index("source_id")?;
                match row.get_ref(index)? {
                    rusqlite::types::ValueRef::Null => None,
                    _ => Some(crate::sql::source_id(row, index)?),
                }
            },
            audio_segment_id: row.get("audio_segment_id")?,
            words_checked: row.get::<_, Option<i64>>("words_checked")? == Some(1),
            words: {
                let stored: Option<String> = row.get("word_timings")?;
                match stored {
                    Some(json) => serde_json::from_str(&json).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                    None => Vec::new(),
                }
            },
        })
    }
}

/// Who a line is put down to, as stored.
#[derive(Debug, Clone, PartialEq)]
pub enum Attribution {
    /// A name a person gave, to the line or to its voice.
    Named(Name),
    /// The voiceprint's best match, and how sure.
    Guessed { name: Name, score: f64 },
}

impl Attribution {
    /// A name or a guess with its score, never both nor half of one; and a
    /// name exactly as [`Name`] spells it, so the wire repeats what is stored.
    fn read(row: &Row<'_>) -> rusqlite::Result<Option<Self>> {
        let label: Option<String> = row.get("speaker_label")?;
        let guess: Option<String> = row.get("speaker_guess")?;
        let score: Option<f64> = row.get("speaker_score")?;
        let refuse = |why: String| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, why.into())
        };
        let name = |raw: String| {
            Name::new(&raw)
                .filter(|n| n.as_str() == raw)
                .ok_or_else(|| refuse(format!("not a name as stored: {raw:?}")))
        };
        match (label, guess, score) {
            (None, None, None) => Ok(None),
            (Some(label), None, None) => Ok(Some(Self::Named(name(label)?))),
            (None, Some(guess), Some(score)) => Ok(Some(Self::Guessed {
                name: name(guess)?,
                score,
            })),
            half => Err(refuse(format!("a speaker stored half: {half:?}"))),
        }
    }

    /// The name, if a person gave it.
    #[must_use]
    pub fn named(&self) -> Option<&Name> {
        match self {
            Self::Named(name) => Some(name),
            Self::Guessed { .. } => None,
        }
    }
}

/// The line as the app shows it, under its own speaker.
pub fn to_out(segment: &Segment) -> TranscriptOut {
    to_out_with(segment, segment.speaker.as_ref())
}

/// The line under `speaker`: its own, or for a folded moment the one
/// [`crate::conversations::shown_speaker`] chose.
pub fn to_out_with(segment: &Segment, speaker: Option<&Attribution>) -> TranscriptOut {
    TranscriptOut {
        id: segment.id,
        // The stored spelling: chrono's could only change it (a `Z` for
        // `+00:00`, dropped microseconds).
        start: segment.start_utc.to_string(),
        end: segment.end_utc.to_string(),
        text: segment.text.clone(),
        language: segment.language.clone(),
        speaker: speaker.map(SpeakerOut::from),
        confidence: segment.asr_confidence,
        model: segment.asr_model.clone(),
        tier: segment.tier,
        hidden: segment.hidden_reason.map(|r| r.as_str().to_owned()),
        hidden_as: segment.hidden_reason.map(HiddenKind::from),
        audio_url: format!("/api/audio/{}", segment.id),
        source: segment.source_id.as_ref().map(|s| s.as_str().to_owned()),
        cluster: segment.speaker_cluster.clone(),
        words_checked: segment.words_checked,
    }
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

/// The tables one person may see, as temporary views of the same names: an
/// unqualified name finds `temp` before `main`, so every query on this
/// connection sees only the viewer's sources, lines, corrections, names and
/// glossary.
/// Live lines carry no source; they are the Mac's own mic's (a `coreaudio`
/// source), so its owner sees them.
const VIEWER_SCHEMA: &str = "
    CREATE TEMP VIEW sources AS
        SELECT * FROM main.sources WHERE owner = (SELECT owner FROM temp.viewer);
    CREATE TEMP VIEW rendered_lines AS
        SELECT * FROM main.rendered_lines
        WHERE source_id IN (SELECT id FROM temp.sources)
           OR (source_id IS NULL AND live_id IS NOT NULL
               AND EXISTS (SELECT 1 FROM temp.sources WHERE kind = 'coreaudio'));
    CREATE TEMP VIEW live_lines AS
        SELECT * FROM main.live_lines
        WHERE EXISTS (SELECT 1 FROM temp.sources WHERE kind = 'coreaudio');
    CREATE TEMP VIEW audio_segments AS
        SELECT * FROM main.audio_segments WHERE source_id IN (SELECT id FROM temp.sources);
    CREATE TEMP VIEW deleted_segments AS
        SELECT * FROM main.deleted_segments WHERE source_id IN (SELECT id FROM temp.sources);
    CREATE TEMP VIEW edits AS
        SELECT * FROM main.edits
        WHERE source_id IN (SELECT id FROM temp.sources)
           OR clip_id IN (SELECT clip_id FROM temp.rendered_lines);
    CREATE TEMP VIEW rendered_clips AS
        SELECT * FROM main.rendered_clips WHERE clip_id IN (SELECT clip_id FROM temp.rendered_lines);
    CREATE TEMP VIEW speakers AS
        SELECT * FROM main.speakers WHERE owner = (SELECT owner FROM temp.viewer);
    CREATE TEMP VIEW vocabulary AS
        SELECT * FROM main.vocabulary WHERE owner = (SELECT owner FROM temp.viewer);
    CREATE TEMP VIEW speaker_embeddings AS
        SELECT * FROM main.speaker_embeddings WHERE speaker_id IN (SELECT id FROM temp.speakers);
";

/// One thing a request names, to check the asker may see it.
#[derive(Debug, Clone)]
pub enum Target {
    Line(i64),
    /// A naming or correction act.
    Edit(i64),
    /// A source.
    Source(String),
    /// A glossary term.
    Term(i64),
    /// A recorder's status rows, by its id (its source's): the owner's, or,
    /// with no source of that id at all, a stray row anyone may clear.
    Device(String),
    /// A clip, by its file name in the ingest plane.
    Clip(String),
}

/// Whether every target is the viewer's, on a connection from [`open_as`].
///
/// # Errors
/// If either database refuses.
pub fn sees(meaning: &Connection, root: &Path, targets: &[Target]) -> rusqlite::Result<bool> {
    for target in targets {
        let seen: bool = match target {
            Target::Line(id) => SEES_LINE.query_row(meaning, [id], |r| r.get(0))?,
            Target::Edit(id) => SEES_EDIT.query_row(meaning, [id], |r| r.get(0))?,
            Target::Source(id) => SEES_SOURCE.query_row(meaning, [id], |r| r.get(0))?,
            Target::Device(id) => SEES_DEVICE.query_row(meaning, [id], |r| r.get(0))?,
            Target::Term(id) => SEES_TERM.query_row(meaning, [id], |r| r.get(0))?,
            Target::Clip(filename) => {
                let ingest = crate::store::open(root)?;
                match crate::clips::by_filename(&ingest, filename)
                    .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?
                {
                    Some(clip) => {
                        SEES_SOURCE.query_row(meaning, [clip.source.as_str()], |r| r.get(0))?
                    }
                    None => false,
                }
            }
        };
        if !seen {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The source ids `viewer` owns, read through [`open_as`]'s views; `None`
/// when no session carried the request, which sees every source. For what
/// lives outside those views: device status, the ingest plane.
///
/// # Errors
/// If the database refuses.
pub fn own_sources(
    conn: &Connection,
    viewer: Option<&crate::webauth::Viewer>,
) -> rusqlite::Result<Option<std::collections::HashSet<String>>> {
    if viewer.is_none() {
        return Ok(None);
    }
    let mut stmt = OWN_SOURCES.prepare(conn)?;
    let ids = stmt.query_map([], |r| r.get(0))?;
    Ok(Some(ids.collect::<rusqlite::Result<_>>()?))
}

/// Refused as not found unless every target is the viewer's; a request no
/// session carried is not checked. Run before a route acts.
///
/// # Errors
/// The response to send instead: 404, or 500 if a database refused.
#[expect(clippy::result_large_err, reason = "the Err is the HTTP response")]
pub async fn check(
    root: &Path,
    viewer: Option<&crate::webauth::Viewer>,
    targets: Vec<Target>,
) -> Result<(), Response> {
    let Some(viewer) = viewer.cloned() else {
        return Ok(());
    };
    let root = root.to_owned();
    let seen = route::blocking("ownership check", move || {
        sees(&open_as(&root, Some(&viewer))?, &root, &targets)
    })
    .await?;
    if seen {
        Ok(())
    } else {
        Err((StatusCode::NOT_FOUND, "not found").into_response())
    }
}

/// [`open`], seeing only what `viewer` owns; everything for a request no
/// session carried (a device, or a recalld run without sign-in).
///
/// # Errors
/// If the database refuses.
#[expect(
    clippy::disallowed_methods,
    reason = "schema, like meaning_schema: the viewer's views, made per connection"
)]
pub fn open_as(
    root: &Path,
    viewer: Option<&crate::webauth::Viewer>,
) -> rusqlite::Result<Connection> {
    let conn = open(root)?;
    if let Some(viewer) = viewer {
        conn.execute_batch("CREATE TEMP TABLE viewer (owner TEXT NOT NULL)")?;
        conn.execute(
            "INSERT INTO temp.viewer (owner) VALUES (?1)",
            [&viewer.user_id],
        )?;
        conn.execute_batch(VIEWER_SCHEMA)?;
    }
    Ok(conn)
}

/// Full-text search over current turns, oldest-first.
pub fn search(conn: &Connection, query: &str, limit: i64) -> rusqlite::Result<ItemsOut> {
    let mut stmt = SEARCH.prepare(conn)?;
    let rows = stmt.query_map((fts_query(query), limit), Segment::from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(to_out(&row?));
    }
    Ok(ItemsOut { items })
}

/// What a person typed, as an FTS5 query: each word quoted, so punctuation is
/// text ("Dr. Visser" was a syntax error); `AND`, `OR`, `NOT` and a trailing
/// `*` keep their meaning.
fn fts_query(typed: &str) -> String {
    typed
        .split_whitespace()
        .map(|word| {
            if matches!(word, "AND" | "OR" | "NOT") {
                return word.to_owned();
            }
            let (stem, prefix) = match word.strip_suffix('*') {
                Some(stem) if !stem.is_empty() => (stem, "*"),
                _ => (word, ""),
            };
            format!("\"{}\"{prefix}", stem.replace('"', "\"\""))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A line by id. A re-render keeps the id of a line whose span did not
/// change; one that changed is gone.
pub fn current_version(conn: &Connection, id: i64) -> rusqlite::Result<Option<TranscriptOut>> {
    BY_ID
        .query_row(conn, [id], Segment::from_row)
        .optional()
        .map(|found| found.as_ref().map(to_out))
}

/// Lines by id, in the order asked and deduplicated.
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
/// The cursors are [`Stamp`]s: they compare with stored instants as text, so
/// they must be spelled as those are.
#[derive(Debug, Default, Clone, Copy)]
pub struct Window<'a> {
    pub before: Option<&'a Stamp>,
    pub after: Option<&'a Stamp>,
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

    let full_page = i64::try_from(segments.len()).is_ok_and(|n| n == limit);
    if full_page && let Some(boundary) = segments.last().map(|s| s.start_utc.clone()) {
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
pub fn timeline(
    conn: &Connection,
    limit: i64,
    before: Option<&Stamp>,
) -> rusqlite::Result<PageOut> {
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
    before: Option<Stamp>,
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
    asked: crate::webauth::Asked,
    Query(q): Query<TimelineQuery>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    route::json("timeline", move || {
        timeline(
            &open_as(&root, viewer.as_ref())?,
            clamp(q.limit),
            q.before.as_ref(),
        )
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
    asked: crate::webauth::Asked,
    Query(q): Query<TranscriptsQuery>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    // A bad id is a 400: dropping it would read as "that turn is gone".
    let mut ids = Vec::new();
    for piece in q.ids.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match piece.parse::<i64>() {
            Ok(id) => ids.push(id),
            Err(_) => return (StatusCode::BAD_REQUEST, "ids must be integers").into_response(),
        }
    }
    let root = st.root.clone();
    route::json("transcripts", move || {
        transcripts(&open_as(&root, viewer.as_ref())?, &ids)
    })
    .await
}

pub async fn review_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    asked: crate::webauth::Asked,
    Query(q): Query<ReviewQuery>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    let limit = clamp(q.limit);
    route::json("review", move || {
        review(
            &open_as(&root, viewer.as_ref())?,
            REVIEW_MAX_CONFIDENCE,
            limit,
        )
    })
    .await
}

pub async fn search_route(
    axum::extract::State(st): axum::extract::State<Arc<State>>,
    asked: crate::webauth::Asked,
    Query(q): Query<SearchQuery>,
) -> Response {
    let viewer = crate::webauth::viewer(asked);
    let root = st.root.clone();
    route::json("search", move || {
        search(&open_as(&root, viewer.as_ref())?, &q.q, clamp(q.limit))
    })
    .await
}
