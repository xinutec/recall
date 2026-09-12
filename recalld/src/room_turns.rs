//! Stage E3's missing half: what a finished `transcribe-room` job MEANS.
//!
//! The runner leases a room block, drives the shim, and retires the job with the
//! shim's reply as opaque JSON (`queue::done`). Measured 2026-09-11: **648 jobs
//! done, 9.6 MB of results, and not one line of either language reads them** —
//! the GPU time is spent and the transcripts exist, unreachable.
//!
//! ⚠ **This module interprets and returns; it does NOT write.** Room turns are
//! gated on #1461 accepting the selection they came from, and putting
//! unvalidated transcripts into the system of record is the one thing that
//! cannot be undone by deleting a row — the archive is what the household said.
//! So the interpretation is built, tested and runnable against the real results
//! now, and the write is a separate decision with a separate commit.
//!
//! ⚠ **"Hidden" was considered and rejected as the safe option.** It is not
//! absent: a hidden row is still in `transcript_fts` (maintained in CODE here,
//! not by a trigger), still counted, and still seen by supersession — which is
//! the machinery whose failure overwrites a person's typed correction. A second
//! writer into that span is not a small thing to guess at.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
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

/// Why a stored result yields no turns. All of these are ordinary, not faults:
/// a refusal and a silent block are both things the fleet expects to see.
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
/// `block_start` comes from the room block's FILENAME, which is the archive's
/// naming contract (`room-YYYYMMDDTHHMMSS.flac`) — the shim's offsets are
/// relative to the clip it was handed and mean nothing on their own.
///
/// ⚠ **A turn with no word in it is dropped here**, not left for a later sweep.
/// Transcribing near-silence does not return nothing, it returns inventions:
/// measured on this very queue, a silent minute came back as "Thank you." twice
/// and another as a 150-character run of tildes (#1410). The queue already
/// refuses MEASURED silence a job; this is the same rule one stage later, for
/// the blocks whose silence nobody had measured yet.
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

/// A span a person has corrected. The one thing in this archive that is not
/// re-derivable from audio.
#[derive(Debug, Clone, PartialEq)]
pub struct Corrected {
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
    /// Room turns declined, and why. Recorded rather than dropped silently:
    /// a refusal nobody can read is indistinguishable from a bug.
    pub refused: Vec<String>,
    /// Room turns the model produced and the quality rules swept — loops and
    /// wordless text. Counted SEPARATELY from `refused` on purpose: a refusal
    /// says a person's words are in the way, a sweep says the model failed, and
    /// a log line that adds them together can report either as the other.
    pub swept: usize,
}

fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && a.1 > b.0
}

/// Decide the write for one block. Pure, so the rules below are testable without
/// a database — they are the rules that can destroy a person's typed words.
///
/// 1. **A room turn overlapping a corrected span is REFUSED.** The human's text
///    stands; a machine pass does not get to restate it.
/// 2. **A per-mic turn overlapping a corrected span is NEVER hidden**, even when
///    a room turn covers it. Hiding is not deleting, but `hidden` is not
///    `absent` either: the row stays in `transcript_fts`, stays counted, and
///    stays visible to supersession.
/// 3. Only a per-mic turn actually covered by an INSERTED room turn is hidden.
/// 4. ⚠ **If nothing will be inserted, nothing is hidden.** This is `refine`'s
///    lesson one stage later: applying the filters AFTER hiding blanked 132
///    segments of real household conversation, including a minute of Dutch about
///    writing things down to remember them. A pass replaces a transcript or it
///    keeps it. It never empties one.
/// 5. **A room turn that is a repetition loop or has no word in it is SWEPT**
///    before any of the above, so it can neither be written nor hide anything.
///
/// ⚠ Rule 5 is placed where it is because of rule 4, not beside it. The whole
/// point of sweeping here — rather than on the read path, where `recall.cleanup`
/// sweeps the per-mic corpus — is that a block whose room turns are ALL junk
/// then inserts nothing, and therefore hides nothing, and the per-mic
/// transcript of that minute survives untouched. Sweeping after the hide set was
/// built would be the 132-segment mistake with a different filter.
///
/// ⚠ It is also what makes the room-vs-per-mic comparison fair. The 2026-09-11
/// measurement put 22% repetition loops against the per-mic corpus's 0% — but
/// that corpus is SWEPT of exactly these and the room turns were written raw, so
/// the number compared raw to swept rather than room audio to mic audio (#1388).
#[must_use]
pub fn plan(room: Vec<RoomTurn>, standing: &[Standing], human: &[Corrected]) -> Plan {
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

    // Rule 4: no insert, no hide. Checked before the hide set is built at all,
    // so there is no path where a filter empties the insert list afterwards.
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

/// The room stream's shape, taken from the builder's own encode (`-ar 16000 -ac 1`)
/// rather than assumed: these become `audio_segments.sample_rate`/`channels`, and a
/// wrong pair there would make every room clip play at the wrong speed.
pub const ROOM_RATE: i64 = 16_000;
pub const ROOM_CHANNELS: i64 = 1;

/// Register built room blocks in the MEANING plane, so their turns have audio.
///
/// ⚠ **Why this has to exist at all.** `transcript_segments.audio_segment_id` is
/// nullable, so room turns could be written with no audio attached — and they
/// must not be. That id is what `/api/audio/{id}` plays a turn from, so every
/// room turn would be text nobody can listen to, in a product whose whole point
/// is going back to what was said.
///
/// ⚠ **A NEW CLASS OF ROW: isis-only.** Every other `audio_segments` row arrived
/// by push from the Mac's master archive. The room stream is BUILT here and the
/// Mac never sees it, so these rows have no counterpart there and must not be
/// expected to.
///
/// Idempotent by the table's own `UNIQUE (source_id, start_utc)` — the whole
/// backfill can be re-run, and is meant to be.
///
/// # Errors
/// If either database refuses the read or the write.
pub fn register_blocks(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    room_dir: &std::path::Path,
) -> rusqlite::Result<usize> {
    // The FK target. `derived` is not a device: it has no recorder to be deaf, no
    // `.alive` marker, and it inherits whichever microphone's audio won the minute.
    meaning.execute(
        "INSERT OR IGNORE INTO sources (id, name, kind) VALUES (?1, ?2, 'derived')",
        (crate::room::ROOM_SOURCE, "Room"),
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
            // A block whose stamp will not parse cannot get an honest end time.
            // Skipped rather than guessed: the grid is the contract, and a row
            // that is off it is a finding, not something to round.
            tracing::warn!(%filename, %start_raw, "room register: unparseable start");
            continue;
        };
        let start = start.with_timezone(&Utc);
        // Exactly one minute, because the builder works a UTC-ALIGNED GRID
        // (`room::BLOCK_S`) rather than cutting variable segments. This is the one
        // place a duration may be asserted instead of measured.
        let end = start + Duration::seconds(crate::room::BLOCK_S);
        added += meaning.execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                crate::room::ROOM_SOURCE,
                room_dir.join(&filename).to_string_lossy(),
                start.to_rfc3339_opts(SecondsFormat::Micros, false),
                end.to_rfc3339_opts(SecondsFormat::Micros, false),
                ROOM_RATE,
                ROOM_CHANNELS,
            ],
        )?;
    }
    Ok(added)
}

/// Marks a per-mic turn hidden because a room turn now covers its minute.
///
/// A reason, not a flag: `hidden_reason` is what a reader sees when asking why a
/// turn vanished, and "the room stream covers this" is recoverable information
/// where a bare `1` is not.
pub const COVERED_BY_ROOM: &str = "covered by the room stream";

/// Apply a [`Plan`] to one block. ONE transaction: the turns, their search-index
/// rows and the hides land together or not at all.
///
/// ⚠ **The search index is maintained in CODE, not by a trigger.**
/// `transcript_fts` is contentless FTS5 that the writer inserts into by hand
/// (`labels_write` says the same, and says it because forgetting it fails
/// nothing — it just makes the text unfindable by the one route most likely to
/// look for it).
///
/// ⚠ **Idempotent by REFUSING, not by overwriting.** A block whose audio segment
/// already carries turns is left entirely alone: a second pass must never mint
/// duplicates, and must never "fix" a minute a person has since edited. The
/// caller gets `Ok(0)`.
///
/// # Errors
/// If the transaction cannot be taken or any statement fails. Nothing is left
/// half-applied.
pub fn write_block(
    conn: &mut rusqlite::Connection,
    audio_segment_id: i64,
    plan: &Plan,
    model: &str,
    now: &str,
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
        tx.execute(
            "INSERT INTO transcript_segments
                 (audio_segment_id, start_utc, end_utc, text, language,
                  language_confidence, asr_confidence, asr_model, provenance,
                  word_timings, created_utc)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                audio_segment_id,
                turn.start.to_rfc3339_opts(SecondsFormat::Micros, false),
                turn.end.to_rfc3339_opts(SecondsFormat::Micros, false),
                turn.text,
                turn.language,
                turn.confidence,
                model,
                ROOM_PROVENANCE,
                turn.word_timings,
                now,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO transcript_fts (rowid, text) VALUES (?1, ?2)",
            (id, &turn.text),
        )?;
        written += 1;
    }
    for hidden in &plan.hide {
        tx.execute(
            "UPDATE transcript_segments SET hidden_reason = ?1
             WHERE id = ?2 AND hidden_reason IS NULL",
            (COVERED_BY_ROOM, hidden),
        )?;
    }
    tx.commit()?;
    Ok(written)
}

/// What a room turn says about where it came from.
pub const ROOM_PROVENANCE: &str = "room";

/// What one pass did, so a log line can be specific about a write that touches
/// the system of record.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pass {
    pub blocks: usize,
    pub turns: usize,
    pub hidden: usize,
    pub refused: usize,
    pub barren: usize,
    /// Room turns the quality rules swept (see [`plan`] rule 5). The number the
    /// room-vs-per-mic comparison turns on: a pass that sweeps most of what the
    /// model produced is reporting on the AUDIO, not on the filter.
    pub swept: usize,
}

/// Turn stored job results into visible turns, one block at a time.
///
/// The whole chain: a done `transcribe-room` job → [`interpret`] → the standing
/// per-mic turns and the corrections that overlap the block's minute → [`plan`]
/// → [`write_block`].
///
/// ⚠ **Bounded by `limit` on purpose.** This is the first thing in the stage that
/// changes a transcript anybody reads, and a pass that ran away would do it 894
/// times before anyone looked. Small batches, many passes.
///
/// # Errors
/// If either database refuses. A block that cannot be interpreted is counted and
/// skipped, never fatal: one unreadable result must not stop the queue draining.
/// The ledger of blocks a pass has already DECIDED WITHOUT WRITING.
///
/// ⚠ **It records only `refused` and `swept`, and that asymmetry is the design.**
/// A block whose turns were written needs no row — the turns ARE the record, so
/// `write_pass` derives "already done" by asking the meaning plane. That is what
/// makes the 2026-09-11 reversal work: deleting the room turns re-enables those
/// blocks automatically, with no second thing to remember to clear. A block that
/// wrote NOTHING has no such trace, which is why it needs one — without it,
/// refused blocks sit at the head of the queue forever and every pass
/// re-examines them.
///
/// ⚠ Lives in the INGEST plane, keyed on `jobs.filename`. `recall.sqlite`'s
/// schema belongs to `store_schema.py` and its versioned migrations; a second
/// migrator on that file is not worth a bookkeeping table.
///
/// ⚠ **So the reversal is now a TWO-PLANE operation.** See the note on
/// `spawn_room_turn_writer` in `main.rs`: deleting the room turns is no longer
/// enough on its own, the ledger rows go too, or the refused blocks stay decided.
///
/// # Errors
/// If the database refuses.
pub fn ensure_ledger(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS room_turn_ledger (
             filename    TEXT PRIMARY KEY,
             outcome     TEXT NOT NULL,
             decided_utc TEXT NOT NULL
         );",
    )
}

/// A block was decided and wrote nothing. `outcome` is for a person reading the
/// table later, never branched on.
fn ledger(
    conn: &rusqlite::Connection,
    filename: &str,
    outcome: &str,
    now: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO room_turn_ledger (filename, outcome, decided_utc)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![filename, outcome, now],
    )?;
    Ok(())
}

pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    model: &str,
    now: &str,
    limit: usize,
) -> rusqlite::Result<Pass> {
    ensure_ledger(ingest)?;
    // ⚠ NO `LIMIT` in the SQL, and `limit` counts blocks DECIDED rather than
    // blocks looked at. A limited query returns the same rows every pass when
    // they are all ineligible — which is the bug this replaces: `ORDER BY
    // filename DESC LIMIT 20` re-examined the newest twenty blocks every two
    // minutes and refused each time, so 49 minutes of running produced exactly
    // the first pass's 73 turns.
    //
    // ⚠ ASCENDING, so a backfill drains FORWARD from the oldest undecided block.
    // Descending means the newest minute is transcribed first and the archive is
    // never reached.
    let mut stmt = ingest.prepare(
        "SELECT j.filename, j.result FROM jobs j
         WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
           AND j.filename NOT IN (SELECT filename FROM room_turn_ledger)
         ORDER BY j.filename ASC",
    )?;
    let jobs: Vec<(String, String)> = stmt
        .query_map(rusqlite::params![crate::queue::TRANSCRIBE_ROOM], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<Result<_, _>>()?;

    let mut pass = Pass::default();
    for (filename, result) in jobs {
        if pass.blocks >= limit {
            break;
        }
        let Some(block_start) = audiocore::names::parse_segment_start(&filename) else {
            // Permanent: a name that is not a segment name never becomes one.
            pass.barren += 1;
            ledger(ingest, &filename, "unnameable", now)?;
            continue;
        };
        // The block's own audio segment. Absent means the registrar has not run
        // for it yet — a reason to wait, never to write a turn with no audio.
        //
        // ⚠ **The one barren cause that gets NO ledger row.** It is the only
        // transient one, and a row here would retire a block permanently for
        // being examined a few seconds too early.
        let Ok(audio_id) = meaning.query_row(
            "SELECT id FROM audio_segments WHERE source_id = ?1 AND start_utc LIKE ?2",
            rusqlite::params![
                crate::room::ROOM_SOURCE,
                format!("{}%", block_start.format("%Y-%m-%dT%H:%M:%S"))
            ],
            |r| r.get::<_, i64>(0),
        ) else {
            pass.barren += 1;
            continue;
        };
        // Already written. Derived rather than ledgered, so a reversal that
        // deletes the room turns makes this block eligible again by itself.
        let written_already: i64 = meaning.query_row(
            "SELECT count(*) FROM transcript_segments WHERE audio_segment_id = ?1",
            [audio_id],
            |r| r.get(0),
        )?;
        if written_already > 0 {
            continue;
        }
        let Ok(turns) = interpret(block_start, &result) else {
            // Permanent: the stored result is what the shim sent and will not
            // change shape on a later pass.
            pass.barren += 1;
            ledger(ingest, &filename, "unreadable", now)?;
            continue;
        };
        let block_end = block_start + Duration::seconds(crate::room::BLOCK_S);
        let standing = standing_between(meaning, block_start, block_end)?;
        let human = corrected_between(meaning, block_start, block_end)?;
        let decided = plan(turns, &standing, &human);
        pass.refused += decided.refused.len();
        pass.swept += decided.swept;
        pass.hidden += decided.hide.len();
        let written = write_block(meaning, audio_id, &decided, model, now)?;
        pass.turns += written;
        pass.blocks += 1;
        if written == 0 {
            // Decided, and left no trace in the meaning plane to derive that
            // from. Without this row the block is indistinguishable from one
            // nobody has looked at, and every later pass reaches it first.
            ledger(ingest, &filename, "nothing-to-write", now)?;
        }
    }
    Ok(pass)
}

/// The per-mic machine turns standing on a span. Room turns are excluded: this
/// asks what the MICROPHONES said, and a previous room turn is not that.
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
            start.to_rfc3339_opts(SecondsFormat::Micros, false),
            end.to_rfc3339_opts(SecondsFormat::Micros, false),
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

/// The spans a person has corrected. Read WIDE and filtered in `plan` rather than
/// trusted to SQL: this is the set whose loss is permanent.
fn corrected_between(
    conn: &rusqlite::Connection,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> rusqlite::Result<Vec<Corrected>> {
    let mut stmt = conn.prepare(
        "SELECT start_utc, end_utc FROM corrections
         WHERE start_utc < ?2 AND end_utc > ?1",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![
            start.to_rfc3339_opts(SecondsFormat::Micros, false),
            end.to_rfc3339_opts(SecondsFormat::Micros, false),
        ],
        |r| {
            Ok(Corrected {
                start: parse_stamp(&r.get::<_, String>(0)?),
                end: parse_stamp(&r.get::<_, String>(1)?),
            })
        },
    )?;
    rows.collect()
}

/// An unparseable stamp becomes the far past, which makes it overlap nothing it
/// should not — a correction that cannot be read must not silently widen into a
/// veto over the whole archive, nor vanish into one that protects nothing.
fn parse_stamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw).map_or(DateTime::<Utc>::MIN_UTC, |t| t.with_timezone(&Utc))
}

/// What a room turn records as its model.
///
/// The `asr` shim's default, named here rather than threaded from the job: the
/// queue does not yet carry a model field, and a turn claiming a model it was not
/// produced by is worse than one naming the only model that runs.
pub const ROOM_MODEL: &str = "mlx-whisper/large-v3-turbo (room)";
