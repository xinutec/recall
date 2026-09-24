//! Turns finished transcription jobs into transcript rows.
//!
//! The runner retires each job with the shim's reply as opaque JSON
//! (`queue::done`); this reads it. A [`Stream`] says which job kind a pass
//! drains, what its rows record as provenance and model, and whether a written
//! turn hides what it covers. Everything else is shared.
//!
//! [`ROOM`] drains `transcribe-room` and is the only stream that hides: a room
//! turn stands in for the microphones on that minute. Its writer is not started
//! (the call in `main` is commented out). [`PER_MIC`] drains
//! `transcribe-segment`, hides nothing, and fills clips that have no turns.
//!
//! Every row a pass writes is deletable by `provenance = '<stream>'` and by
//! nothing else; a NULL provenance could not be taken back.
//!
//! Hiding is not safer than deleting: a hidden row is still in `transcript_fts`,
//! still counted, and still seen by supersession.

use crate::turn_store::{self, HiddenReason, NewTurn, Protected, Provenance};
use audiocore::instant;
use audiocore::instant::Stamp;
use audiocore::job::Kind;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

/// One turn a room block's transcript implies, in the archive's own terms.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomTurn {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub text: String,
    pub language: Option<String>,
    pub confidence: Option<f64>,
    /// The shim's per-word timings, verbatim, or `None` when it sent none.
    pub word_timings: Option<String>,
}

/// Why a stored result yields no turns. All are ordinary outcomes, not faults.
#[derive(Debug, PartialEq)]
pub enum Barren {
    /// The shim reported failure (`ok: false`). The clip is the problem.
    Refused,
    /// Valid JSON, no segments — a block with nothing said in it.
    NothingSaid,
    /// The stored result is not the shape this understands.
    Unreadable(String),
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    result: Option<Outcome>,
}

#[derive(Deserialize)]
struct Outcome {
    language: Option<String>,
    #[serde(default)]
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
    confidence: Option<f64>,
    #[serde(default)]
    words: Option<serde_json::Value>,
}

/// Seconds-from-block-start to an absolute instant.
fn at(block_start: DateTime<Utc>, offset_s: f64) -> DateTime<Utc> {
    block_start + Duration::milliseconds((offset_s * 1000.0).round() as i64)
}

/// Interpret one stored job result as the turns it implies.
///
/// `block_start` comes from the clip's filename; the shim's offsets are relative
/// to the clip and mean nothing on their own.
///
/// A segment with no alphanumeric character, or with no duration, is dropped
/// here: near-silence comes back as invented text (e.g. "Thank you." or a run of
/// tildes), not as nothing.
pub fn interpret(block_start: DateTime<Utc>, stored: &str) -> Result<Vec<RoomTurn>, Barren> {
    let reply: Reply =
        serde_json::from_str(stored).map_err(|e| Barren::Unreadable(e.to_string()))?;
    if !reply.ok {
        return Err(Barren::Refused);
    }
    let outcome = reply.result.ok_or(Barren::NothingSaid)?;
    let turns: Vec<RoomTurn> = outcome
        .segments
        .into_iter()
        .filter(|s| s.text.chars().any(char::is_alphanumeric))
        .filter(|s| s.end > s.start)
        .map(|s| RoomTurn {
            start: at(block_start, s.start),
            end: at(block_start, s.end),
            text: s.text.trim().to_owned(),
            language: outcome.language.clone(),
            confidence: s.confidence,
            word_timings: s.words.as_ref().map(std::string::ToString::to_string),
        })
        .collect();
    if turns.is_empty() {
        return Err(Barren::NothingSaid);
    }
    Ok(turns)
}

/// A machine turn already standing on this block's minute, per microphone.
#[derive(Debug, Clone, PartialEq)]
pub struct Standing {
    pub id: i64,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// What a write would do, decided before anything is written.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// Room turns to insert.
    pub insert: Vec<RoomTurn>,
    /// Per-mic turn ids to hide, because a written room turn covers them.
    pub hide: Vec<i64>,
    /// Room turns declined, and why, so a refusal is visible rather than silent.
    pub refused: Vec<String>,
    /// Room turns swept by the quality rules (loops, wordless text). Kept apart
    /// from `refused`: a refusal means a person's words are in the way, a sweep
    /// means the model failed.
    pub swept: usize,
}

fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && a.1 > b.0
}

/// Decide the write for one block. Pure, so the rules that protect a person's
/// typed words are testable without a database.
///
/// 1. A room turn overlapping a corrected span is refused: the human text stands.
/// 2. A per-mic turn overlapping a corrected span is never hidden, even when a
///    room turn covers it.
/// 3. Only a per-mic turn covered by an inserted room turn is hidden.
/// 4. If nothing will be inserted, nothing is hidden. A pass replaces a
///    transcript or keeps it; it never empties one.
/// 5. A room turn that is a repetition loop or wordless is swept before any of
///    the above, so it can neither be written nor hide anything.
///
/// ⚠ Rule 5 must run before the hide set is built: a block whose room turns are
/// all junk then inserts nothing, so rule 4 keeps its per-mic transcript. It
/// also keeps room turns comparable with the per-mic corpus, which is swept of
/// the same junk.
#[must_use]
pub fn plan(room: Vec<RoomTurn>, standing: &[Standing], human: &[Protected]) -> Plan {
    let hits_human = |span: (DateTime<Utc>, DateTime<Utc>)| {
        human.iter().any(|c| overlaps(span, (c.start, c.end)))
    };

    let mut out = Plan::default();
    for turn in room {
        if crate::quality::is_repetition_loop(&turn.text) || crate::quality::is_wordless(&turn.text)
        {
            out.swept += 1;
            continue;
        }
        if hits_human((turn.start, turn.end)) {
            out.refused.push(format!(
                "human-corrected span {}..{} — the person's text stands",
                turn.start.to_rfc3339(),
                turn.end.to_rfc3339()
            ));
            continue;
        }
        out.insert.push(turn);
    }

    // Rule 4: no insert, no hide.
    if out.insert.is_empty() {
        return out;
    }

    for candidate in standing {
        let span = (candidate.start, candidate.end);
        if hits_human(span) {
            continue; // rule 2
        }
        if out
            .insert
            .iter()
            .any(|written| overlaps(span, (written.start, written.end)))
        {
            out.hide.push(candidate.id);
        }
    }
    out
}

/// The room stream's shape, from the builder's encode (`-ar 16000 -ac 1`). These
/// become `audio_segments.sample_rate`/`channels`; a wrong pair plays every room
/// clip at the wrong speed.
pub const ROOM_RATE: i64 = 16_000;
pub const ROOM_CHANNELS: i64 = 1;

/// Register built room blocks in the meaning plane, so their turns have audio.
///
/// `transcript_segments.audio_segment_id` is nullable, but a room turn without
/// one could not be played: `/api/audio/{id}` plays a turn from that id.
///
/// The room stream is built on this host, so these rows have no counterpart in
/// the Mac's archive.
///
/// Idempotent by `UNIQUE (source_id, start_utc)`, so the backfill can be re-run.
///
/// # Errors
/// If either database refuses the read or the write.
pub fn register_blocks(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    room_dir: &std::path::Path,
) -> rusqlite::Result<usize> {
    // The FK target. The room source is not a device: it has no recorder and no
    // `.alive` marker; its audio is whichever microphone won the minute.
    meaning.execute(
        "INSERT OR IGNORE INTO sources (id, name, kind) VALUES (?1, ?2, ?3)",
        (crate::room::ROOM_SOURCE, "Room", crate::room::ROOM_KIND),
    )?;

    let mut stmt = ingest
        .prepare("SELECT filename, start_utc FROM segments WHERE source = ?1 ORDER BY start_utc")?;
    let rows = stmt.query_map([crate::room::ROOM_SOURCE], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut added = 0;
    for row in rows {
        let (filename, start_raw) = row?;
        let Ok(start) = DateTime::parse_from_rfc3339(&start_raw) else {
            // No honest end time without a start. Skipped rather than rounded.
            tracing::warn!(%filename, %start_raw, "room register: unparseable start");
            continue;
        };
        let start = start.with_timezone(&Utc);
        // Asserted, not measured: the builder cuts a UTC-aligned grid of
        // `room::BLOCK_S`.
        let end = start + Duration::seconds(crate::room::BLOCK_S);
        added += meaning.execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                crate::room::ROOM_SOURCE,
                room_dir.join(&filename).to_string_lossy(),
                instant::python_isoformat_utc(start),
                instant::python_isoformat_utc(end),
                ROOM_RATE,
                ROOM_CHANNELS,
            ],
        )?;
    }
    Ok(added)
}

/// Which pass decided a clip: a job kind's, or registration's, which has no job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    Job(Kind),
    Register,
}

impl PassKind {
    /// The stored spelling in `pass_ledger.kind`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Job(kind) => kind.as_str(),
            Self::Register => "register-segment",
        }
    }
}

impl From<Kind> for PassKind {
    fn from(kind: Kind) -> Self {
        Self::Job(kind)
    }
}

impl rusqlite::ToSql for PassKind {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.as_str().into())
    }
}

/// What one registrar pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registered {
    /// Clips given an `audio_segments` row, and therefore somewhere to hang a turn.
    pub added: usize,
    /// Clips whose source the meaning plane does not know yet. The only
    /// non-terminal outcome: everything else is ledgered and not revisited.
    pub waiting: usize,
    /// Clips with an unparseable name or that ffmpeg could not read. Ledgered.
    pub unreadable: usize,
    /// Clips whose minute a sibling file (e.g. the `.wav` beside the `.opus`)
    /// already holds under `UNIQUE (source_id, start_utc)`. Common, not a fault;
    /// counted apart from `added` so the duplicates stay visible.
    pub covered: usize,
    /// Clips already registered by name, retired cheaply.
    pub retired: usize,
    /// Clips whose file was decoded: the pass's whole cost. Lets a test assert
    /// that a duplicate costs no decode.
    pub probed: usize,
}

/// Register microphone clips from the ingest plane in the meaning plane, so
/// their turns have audio to hang on.
///
/// The path is the ingest copy (`<root>/ingest/<source>/`), the complete one.
/// `INSERT OR IGNORE` on `UNIQUE (source_id, start_utc)` means an already
/// registered clip keeps its path.
///
/// `end_utc` is decoded, not assumed: a microphone clip is whatever the segment
/// muxer closed, and `write_pass` sizes the human-correction window from it.
///
/// A clip whose source is unknown waits: `sources.kind` is the sender's to
/// declare, and registering under a guess is permanent.
///
/// `limit` bounds probes, because a probe decodes the whole file. Cheap terminal
/// decisions are not charged against it, so a backlog of them drains in one pass.
///
/// ⚠ Every terminal outcome must be ledgered. The candidate query is "not in the
/// ledger", so an unledgered decision is re-made, and possibly re-decoded, on
/// every pass.
///
/// # Errors
/// If either database refuses.
pub fn register_segments(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    root: &std::path::Path,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Registered> {
    // Uploads are excluded because `upload::register` writes their meaning-plane
    // rows; they are transcribed like any microphone clip.
    let mics: std::collections::HashSet<String> = {
        let mut stmt =
            meaning.prepare("SELECT id FROM sources WHERE kind NOT IN ('upload', ?1)")?;
        let rows = stmt.query_map([crate::room::ROOM_KIND], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    // Read once into sets rather than looked up per candidate; see
    // `registered_names`.
    let have = registered_names(meaning)?;
    let mut minutes = registered_minutes(meaning)?;

    // Newest first, unlike `write_pass`: a clip arriving now must not wait
    // behind a backfill, and the ledger retires what cannot be read, so nothing
    // starves.
    let candidates: Vec<(String, String)> = {
        let mut stmt = ingest.prepare(
            "SELECT s.filename, s.source FROM segments s
             WHERE s.source != ?1
               AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                               WHERE l.kind = ?2 AND l.filename = s.filename)
             ORDER BY s.start_utc DESC",
        )?;
        let rows = stmt.query_map((crate::room::ROOM_SOURCE, PassKind::Register), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let mut out = Registered::default();
    let mut probes = 0;
    for (filename, source) in candidates {
        // The budget bounds decodes only; cheap decisions below do not spend it.
        if probes >= limit {
            break;
        }
        // Already registered. Terminal, so ledgered.
        if have.contains(&filename) {
            ledger(
                ingest,
                PassKind::Register,
                &filename,
                "already-registered",
                now,
            )?;
            out.retired += 1;
            continue;
        }
        // ⚠ Not ledgered, unlike every other outcome: the source may be
        // registered later, and a ledgered clip would never come back.
        if !mics.contains(&source) {
            out.waiting += 1;
            continue;
        }
        let Some(start) = audiocore::names::parse_segment_start(&filename) else {
            out.unreadable += 1;
            ledger(ingest, PassKind::Register, &filename, "unnameable", now)?;
            continue;
        };
        // A sibling already holds this minute, so the insert could only be
        // ignored. Decided without decoding.
        let minute = (source.clone(), instant::python_isoformat_utc(start));
        if minutes.contains(&minute) {
            out.covered += 1;
            ledger(
                ingest,
                PassKind::Register,
                &filename,
                "covered-by-sibling",
                now,
            )?;
            continue;
        }
        let path = crate::store::source_dir(root, &source).join(&filename);
        probes += 1;
        out.probed += 1;
        let Ok(media) = crate::upload::probe(&path) else {
            // Permanent: e.g. a header-only file from a dead capture holds no
            // audio and never will.
            out.unreadable += 1;
            ledger(ingest, PassKind::Register, &filename, "unreadable", now)?;
            continue;
        };
        let end = start + Duration::microseconds((media.duration_s * 1e6).round() as i64);
        let inserted = meaning.execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                source,
                path.to_string_lossy(),
                instant::python_isoformat_utc(start),
                instant::python_isoformat_utc(end),
                media.sample_rate,
                media.channels,
            ],
        )?;
        // An ignored insert means a sibling holds this minute: terminal, and
        // ledgered like a registration.
        if inserted == 1 {
            out.added += 1;
            // `minutes` is a snapshot from before the loop; without this, a
            // sibling later in the same pass would pay a full decode.
            minutes.insert(minute);
        } else {
            out.covered += 1;
        }
        let outcome = if inserted == 1 {
            "registered"
        } else {
            "covered-by-sibling"
        };
        ledger(ingest, PassKind::Register, &filename, outcome, now)?;
    }
    Ok(out)
}

/// Every registered clip's basename. One pass over the column, because a
/// correlated lookup per candidate scans both tables and takes minutes.
fn registered_names(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut stmt = meaning.prepare("SELECT path FROM audio_segments")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut set = std::collections::HashSet::new();
    for path in rows {
        if let Some(name) = path?.rsplit('/').next() {
            set.insert(name.to_owned());
        }
    }
    Ok(set)
}

/// Every registered `(source_id, start_utc)`: the key the `UNIQUE` constraint
/// rejects on. A sibling file (the `.wav` beside the `.opus`) misses
/// [`registered_names`], but its start time is in its name, so this catches it
/// before a decode.
fn registered_minutes(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<(String, String)>> {
    let mut stmt = meaning.prepare("SELECT source_id, start_utc FROM audio_segments")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

/// Apply a [`Plan`] to one block in one transaction: the turns, their search
/// rows and the hides land together or not at all.
///
/// Idempotent by refusing: a block whose audio segment already carries turns is
/// left alone and the caller gets `Ok(0)`, so a second pass neither duplicates
/// turns nor overwrites an edited minute.
///
/// # Errors
/// If the transaction cannot be taken or any statement fails. Nothing is left
/// half-applied.
pub fn write_block(
    conn: &mut rusqlite::Connection,
    audio_segment_id: i64,
    span: (DateTime<Utc>, DateTime<Utc>),
    plan: &Plan,
    stream: &Stream,
    now: &Stamp,
) -> rusqlite::Result<usize> {
    if plan.insert.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    let already: i64 = tx.query_row(
        "SELECT count(*) FROM transcript_segments WHERE audio_segment_id = ?1",
        [audio_segment_id],
        |row| row.get(0),
    )?;
    if already > 0 {
        return Ok(0);
    }
    let mut written = 0;
    for turn in &plan.insert {
        let (start, end) = (Stamp::of(turn.start), Stamp::of(turn.end));
        // Implausibly slow speech zeroes the confidence, as in `diarized`;
        // `word_spans` reads both timing encodings.
        let confidence = match turn.word_timings.as_deref() {
            Some(timings)
                if crate::quality::is_implausibly_slow(&crate::quality::word_spans(timings)) =>
            {
                Some(0.0)
            }
            _ => turn.confidence,
        };
        turn_store::insert(
            &tx,
            &NewTurn {
                audio_segment_id: Some(audio_segment_id),
                language: turn.language.as_deref(),
                asr_confidence: confidence,
                asr_model: Some(stream.model),
                provenance: Some(stream.provenance.clone()),
                word_timings: turn.word_timings.as_deref(),
                created_utc: Some(now),
                ..NewTurn::at(&start, &end, &turn.text)
            },
        )?;
        written += 1;
    }
    if stream.reconciles_live {
        let (from, to) = span;
        turn_store::reconcile_live(
            &tx,
            &instant::python_isoformat_utc(from),
            &instant::python_isoformat_utc(to),
        )?;
    }
    for hidden in &plan.hide {
        turn_store::hide(&tx, *hidden, &HiddenReason::CoveredByRoom)?;
    }
    tx.commit()?;
    Ok(written)
}

/// Which transcription stream a pass drains, and what differs between streams.
///
/// ⚠ A `Stream` is the unit of reversal: `DELETE FROM transcript_segments WHERE
/// provenance = '<stream>'` must name exactly the rows its passes wrote, so
/// `provenance` must be unique per stream and never NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream<'a> {
    /// The queue job kind whose stored results this pass interprets.
    pub kind: Kind,
    /// What the written rows record in `provenance`: the reversal key.
    pub provenance: Provenance,
    /// What they record in `asr_model`.
    pub model: &'a str,
    /// Whether writing turns for a clip also hides the provisional live turns
    /// on the same span.
    ///
    /// Required for the stream that acts as the archive pass: the archive turn
    /// supersedes the live guess, and without this the timeline shows both.
    pub reconciles_live: bool,
    /// Whether a written turn hides the per-mic turns it covers.
    ///
    /// True for the room stream alone, which is why [`plan`]'s rules 2 to 4
    /// exist. A per-mic pass fills clips that have no turns, so it has nothing
    /// to hide.
    pub hides_covered: bool,
}

/// The derived one-microphone-per-minute stream (`transcribe-room`).
pub const ROOM: Stream<'static> = Stream {
    kind: Kind::TranscribeRoom,
    provenance: Provenance::Room,
    model: ROOM_MODEL,
    // The room stream is derived from microphones whose own pass reconciles the
    // live turns; doing it again would make the reversal ambiguous.
    reconciles_live: false,
    hides_covered: true,
};

/// What the `asr` shim loads when the caller names no model:
/// `recall.asr.DEFAULT_MODEL`.
///
/// ⚠ The queue carries no model field, so this copy must match the Python one.
/// The test `a_per_mic_turn_names_the_model_the_shim_will_actually_load` reads
/// `asr.py` and fails on a mismatch.
pub const SHIM_MODEL: &str = "mlx-community/whisper-large-v3-turbo";

/// One microphone's own clip (`transcribe-segment`).
///
/// `model` is the shim's real default, not a decorated name like [`ROOM`]'s:
/// these rows join the existing per-microphone corpus, and filtering on
/// `asr_model` must not split it by writer. `provenance` says who wrote them.
pub const PER_MIC: Stream<'static> = Stream {
    kind: Kind::TranscribeSegment,
    provenance: Provenance::PerMic,
    model: SHIM_MODEL,
    // This is the archive pass, so it reconciles live turns.
    reconciles_live: true,
    hides_covered: false,
};

/// What one pass did, for the log line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub blocks: usize,
    pub turns: usize,
    pub hidden: usize,
    pub refused: usize,
    pub barren: usize,
    /// Turns the quality rules swept (see [`plan`] rule 5).
    pub swept: usize,
}

/// Whether the block starting at `block_start` on `source` was deliberately
/// deleted, matched on the same timestamp spelling the audio lookup uses.
///
/// # Errors
/// If the meaning plane refuses.
pub fn tombstoned_block(
    meaning: &rusqlite::Connection,
    source: &str,
    block_start: DateTime<Utc>,
) -> rusqlite::Result<bool> {
    use rusqlite::OptionalExtension;
    Ok(meaning
        .query_row(
            "SELECT 1 FROM deleted_segments WHERE source_id = ?1 AND start_utc = ?2",
            rusqlite::params![source, instant::python_isoformat_utc(block_start)],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// Record a pass's terminal decision on a clip. `outcome` is for a person
/// reading the table, never branched on.
///
/// `write_pass` ledgers only clips it wrote nothing for: written turns are their
/// own record, so deleting a stream's turns re-enables those clips. A reversal
/// must therefore clear both the turns and these rows.
pub fn ledger(
    conn: &rusqlite::Connection,
    kind: impl Into<PassKind>,
    filename: &str,
    outcome: &str,
    now: &Stamp,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pass_ledger (kind, filename, outcome, decided_utc)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![kind.into(), filename, outcome, now],
    )?;
    Ok(())
}

/// Turn stored job results into visible turns, one clip at a time:
/// [`interpret`] → [`plan`] → [`write_block`].
///
/// ⚠ `limit` counts clips decided, and the SQL has no `LIMIT`: a limited query
/// returns the same ineligible rows every pass and never advances.
///
/// Ascending, so a backfill drains forward from the oldest undecided clip.
///
/// # Errors
/// If either database refuses. An uninterpretable result is counted and
/// skipped — one bad clip must not stop the queue draining.
pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    stream: &Stream,
    now: &Stamp,
    limit: usize,
) -> rusqlite::Result<Pass> {
    // ⚠ The source is joined from the ingest plane, never parsed from the
    // filename: `meeting-20260907-0905` is a source id, and no split of a
    // filename on a hyphen is safe.
    let mut stmt = ingest.prepare(
        "SELECT j.filename, j.result, s.source FROM jobs j
         JOIN segments s ON s.filename = j.filename
         WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                           WHERE l.kind = ?1 AND l.filename = j.filename)
         ORDER BY s.start_utc ASC, j.filename ASC",
    )?;
    let jobs: Vec<(String, String, String)> = stmt
        .query_map(rusqlite::params![stream.kind], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;

    let mut pass = Pass::default();
    for (filename, result, source) in jobs {
        if pass.blocks >= limit {
            break;
        }
        let Some(block_start) = audiocore::names::parse_segment_start(&filename) else {
            // Permanent: a name that is not a segment name never becomes one.
            pass.barren += 1;
            ledger(ingest, stream.kind, &filename, "unnameable", now)?;
            continue;
        };
        // The clip's audio segment. Absent means it is not registered yet: wait,
        // never write a turn that cannot be played.
        //
        // ⚠ Not ledgered: this is the one transient barren cause, and a row
        // would retire the clip for being examined too early.
        let Ok((audio_id, end_raw)) = meaning.query_row(
            "SELECT id, end_utc FROM audio_segments
             WHERE source_id = ?1 AND start_utc = ?2",
            rusqlite::params![source, instant::python_isoformat_utc(block_start)],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ) else {
            pass.barren += 1;
            // Unless the block was deleted: then the audio is never coming.
            if tombstoned_block(meaning, &source, block_start)? {
                ledger(ingest, stream.kind, &filename, "deleted", now)?;
            }
            continue;
        };
        // Already written. Derived rather than ledgered, so deleting the turns
        // makes the clip eligible again.
        let written_already: i64 = meaning.query_row(
            "SELECT count(*) FROM transcript_segments WHERE audio_segment_id = ?1",
            [audio_id],
            |r| r.get(0),
        )?;
        if written_already > 0 {
            continue;
        }
        let Ok(turns) = interpret(block_start, &result) else {
            // Permanent: the stored result will not change.
            pass.barren += 1;
            ledger(ingest, stream.kind, &filename, "unreadable", now)?;
            continue;
        };
        // ⚠ The end comes from the clip's own row, not the room grid: a
        // microphone clip can be short, and a correction window that ends early
        // cannot see a correction it is about to overwrite.
        let Ok(block_end) = DateTime::parse_from_rfc3339(&end_raw) else {
            pass.barren += 1;
            ledger(ingest, stream.kind, &filename, "unspanned", now)?;
            continue;
        };
        let block_end = block_end.with_timezone(&Utc);
        // Read only for a stream that can hide: it is a scan per clip.
        let standing = if stream.hides_covered {
            standing_between(meaning, block_start, block_end)?
        } else {
            Vec::new()
        };
        let human = turn_store::protected_between(meaning, block_start, block_end)?;
        let decided = plan(turns, &standing, &human);
        pass.refused += decided.refused.len();
        pass.swept += decided.swept;
        pass.hidden += decided.hide.len();
        let written = write_block(
            meaning,
            audio_id,
            (block_start, block_end),
            &decided,
            stream,
            now,
        )?;
        pass.turns += written;
        pass.blocks += 1;
        if written == 0 {
            // Decided but left no trace in the meaning plane; without this row
            // every later pass would reach it first.
            ledger(ingest, stream.kind, &filename, "nothing-to-write", now)?;
        }
    }
    Ok(pass)
}

/// The visible per-mic turns overlapping a span. Room turns are excluded.
fn standing_between(
    conn: &rusqlite::Connection,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> rusqlite::Result<Vec<Standing>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.start_utc, t.end_utc FROM transcript_segments t
         JOIN audio_segments a ON a.id = t.audio_segment_id
         WHERE a.source_id != ?1 AND t.hidden_reason IS NULL
           AND t.superseded_by IS NULL
           AND t.start_utc < ?3 AND t.end_utc > ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            crate::room::ROOM_SOURCE,
            instant::python_isoformat_utc(start),
            instant::python_isoformat_utc(end),
        ],
        |r| {
            Ok(Standing {
                id: r.get(0)?,
                start: parse_stamp(&r.get::<_, String>(1)?),
                end: parse_stamp(&r.get::<_, String>(2)?),
            })
        },
    )?;
    rows.collect()
}

/// An unparseable stamp becomes `DateTime::MIN_UTC`. A span with both ends
/// unreadable then overlaps nothing; one with only its start unreadable reaches
/// back to the start of time.
fn parse_stamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw).map_or(DateTime::<Utc>::MIN_UTC, |t| t.with_timezone(&Utc))
}

/// What a room turn records as its model.
///
/// The `asr` shim's default, named here because the queue carries no model field.
pub const ROOM_MODEL: &str = "mlx-whisper/large-v3-turbo (room)";
