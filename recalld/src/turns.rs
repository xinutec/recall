//! What a finished transcription job MEANS.
//!
//! The runner leases a clip, drives the shim, and retires the job with the
//! shim's reply as opaque JSON (`queue::done`). This is what reads it.
//!
//! ⚠ **TWO STREAMS, ONE WRITER, and the difference between them is one field.**
//! A [`Stream`] says which job kind a pass drains, what the rows record as their
//! provenance, and whether a written turn HIDES what it covers. Everything else
//! — interpreting the shim's reply, refusing a human-corrected span, sweeping
//! model junk, the transaction — is the same work and is written once.
//!
//! - [`ROOM`] drains `transcribe-room`. It is the stream whose SELECTION #1461
//!   cannot yet referee, and it is the only one that hides anything: a room turn
//!   standing in for four microphones means those four turns should not also be
//!   read. **Off** — the call in `main` is commented out, with the reason
//!   beside it.
//! - [`PER_MIC`] drains `transcribe-segment`. It hides nothing and replaces
//!   nothing — it writes the turns for microphone clips that have NONE, which is
//!   14,078 of 22,312 of them. This is `worker.py`'s loop moved to the runner,
//!   not a new judgement about audio, so it carries none of the room stream's
//!   open question.
//!
//! ⚠ **The provenance field is the RESTORE, armed before the break.** Every row
//! either pass writes is deletable by `provenance = '<stream>'` and by nothing
//! else — which is what made the 2026-09-11 room reversal a one-line `DELETE`
//! rather than an archaeology problem. A pass that wrote NULL there, matching
//! the corpus convention, would be a pass nobody can take back.
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
/// ⚠ **DO NOT "FIX" THE TIMESTAMP SPELLING HERE.** `SecondsFormat::Micros`
/// writes `...T10:00:00.000000+00:00`, which is not what
/// [`crate::instant::python_isoformat_utc`] would write and not what
/// `register_segments` writes — and that inconsistency is CORRECT, because the
/// idempotency key is compared as TEXT. All 5,645 room rows already carry the
/// fractional spelling (measured 2026-09-13); changing it would make every one
/// of them stop matching and the next pass would mint 5,645 duplicates. The two
/// registrars differ because the rows they are idempotent AGAINST differ.
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

/// The job kind the segment registrar records its refusals under. Not a queue
/// kind — no runner ever leases this — but the ledger is keyed on (kind,
/// filename) and this pass needs its own half of that key.
pub const REGISTER_SEGMENT: &str = "register-segment";

/// What one registrar pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registered {
    /// Clips given an `audio_segments` row, and therefore somewhere to hang a turn.
    pub added: usize,
    /// Clips whose SOURCE the meaning plane does not know. Not a fault and not
    /// a verdict — see the note on `sources` below. The ONLY non-terminal
    /// outcome here: everything else is ledgered and never looked at again.
    pub waiting: usize,
    /// Clips ffmpeg could not read. Ledgered, so a pass reaches past them.
    pub unreadable: usize,
    /// Clips whose minute is ALREADY registered — a sibling file with the same
    /// `(source_id, start_utc)` holds the row, so the insert was ignored.
    ///
    /// ⚠ Not a fault: `.wav` and `.opus` copies of one minute are 1,599 clips
    /// of the archive (#1591). It is counted separately because "the insert did
    /// nothing" and "the clip is new" were indistinguishable before, and that
    /// is what let them be re-decoded for ever.
    pub covered: usize,
    /// Clips an earlier pass had already registered, retired cheaply by name.
    pub retired: usize,
    /// Clips whose file was DECODED — the pass's whole cost, and the only
    /// counter that can show work being done to no effect.
    ///
    /// ⚠ It is here so a test can assert a duplicate costs ZERO of them. That
    /// is a claim about cost, and a claim about cost that is not measured is
    /// the reason this pass decoded 1,599 files a day without anyone noticing.
    pub probed: usize,
}

/// Register microphone clips in the MEANING plane, from the INGEST plane, so
/// their turns have somewhere to hang.
///
/// ⚠ The path is the INGEST copy (`<root>/ingest/<source>/`), which is the
/// complete one; the `<root>/<source>/` mirror is short (#1591). A clip already
/// registered keeps its path — `INSERT OR IGNORE` on `UNIQUE (source_id,
/// start_utc)` — so this never repoints a row out from under playable audio.
///
/// ⚠ `end_utc` is DECODED, not assumed: a microphone clip is whatever the
/// segment muxer closed, and `write_pass` sizes the human-correction window
/// from this column. Too narrow there overwrites somebody's typed words.
///
/// ⚠ An unknown SOURCE waits. `sources.kind` is the sender's to know
/// (`work::store_segment`), and registering under a guess is permanent.
///
/// `limit` bounds PROBES, because probing decodes the whole file. Cheap terminal
/// decisions — a clip already registered, a name that will never parse — are not
/// charged against it, so a backlog of them drains in one pass instead of one
/// clip per pass.
///
/// ⚠ **EVERY terminal outcome is ledgered, and that is the whole of this pass's
/// correctness.** The candidate query is "not in the ledger"; a decision that
/// does not write one leaves the clip a candidate for ever. `write_pass` says
/// the same thing about its own limit, and says it because an earlier version
/// re-examined the newest twenty blocks for ever and never advanced — this pass
/// made the identical mistake in a costlier place. Measured 2026-09-14 against
/// the live fleet: 1,599 clips whose insert was ignored were being decoded in
/// full on every pass, the `.wav` copies at 48 kHz, achieving nothing.
///
/// # Errors
/// If either database refuses.
pub fn register_segments(
    meaning: &rusqlite::Connection,
    ingest: &rusqlite::Connection,
    root: &std::path::Path,
    now: &str,
    limit: usize,
) -> rusqlite::Result<Registered> {
    ensure_ledger(ingest)?;
    // ⚠ Uploads are excluded because `upload::register` already writes their
    // meaning-plane rows — not because they take a different road to a
    // transcriber. They do not: since 2026-09-17 an upload is leased and
    // transcribed exactly as a microphone clip is (#1649).
    let mics: std::collections::HashSet<String> = {
        let mut stmt =
            meaning.prepare("SELECT id FROM sources WHERE kind NOT IN ('upload', ?1)")?;
        let rows = stmt.query_map([crate::room::ROOM_KIND], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>()?
    };
    // Registered already, by BASENAME. One pass over the column rather than a
    // correlated lookup per candidate: the same shape `derive_segment_jobs`
    // uses, and for the same reason — the correlated form was a full scan of
    // both tables and ran ten minutes against the live fleet before it was
    // killed.
    let have = registered_names(meaning)?;
    let mut minutes = registered_minutes(meaning)?;

    // ⚠ NEWEST FIRST, unlike `write_pass`. This pass has no starvation problem
    // to avoid — the ledger retires what it cannot read — and the live clip
    // arriving now must not queue behind 5,487 clips of backfill before anyone
    // can read what was just said (decision 8).
    let candidates: Vec<(String, String)> = {
        let mut stmt = ingest.prepare(
            "SELECT s.filename, s.source FROM segments s
             WHERE s.source != ?1
               AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                               WHERE l.kind = ?2 AND l.filename = s.filename)
             ORDER BY s.start_utc DESC",
        )?;
        let rows = stmt.query_map((crate::room::ROOM_SOURCE, REGISTER_SEGMENT), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<Result<_, _>>()?
    };

    let mut out = Registered::default();
    let mut probes = 0;
    for (filename, source) in candidates {
        // ⚠ The budget bounds DECODES, and only decodes. A cheap decision that
        // consumed it would make a backlog of already-registered clips take one
        // pass each to retire — which is the shape of the bug this pass had.
        if probes >= limit {
            break;
        }
        // Already registered, by an earlier pass or by the Python. Terminal, and
        // it MUST be ledgered: without a row it stays a candidate for ever, and
        // the only thing standing between it and a full decode is this set.
        if have.contains(&filename) {
            ledger(
                ingest,
                REGISTER_SEGMENT,
                &filename,
                "already-registered",
                now,
            )?;
            out.retired += 1;
            continue;
        }
        // ⚠ NOT ledgered, and the only outcome that is not: the source may be
        // registered later, and a clip retired here would never come back.
        if !mics.contains(&source) {
            out.waiting += 1;
            continue;
        }
        let Some(start) = audiocore::names::parse_segment_start(&filename) else {
            out.unreadable += 1;
            ledger(ingest, REGISTER_SEGMENT, &filename, "unnameable", now)?;
            continue;
        };
        // A sibling already holds this minute, so the insert below could only be
        // ignored. Decided WITHOUT decoding: the duration would be discarded.
        let minute = (source.clone(), crate::instant::python_isoformat_utc(start));
        if minutes.contains(&minute) {
            out.covered += 1;
            ledger(
                ingest,
                REGISTER_SEGMENT,
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
            // Permanent as far as this pass is concerned: a header-only
            // dead-capture tombstone holds no audio and never will. Ledgered so
            // the next pass reaches PAST it — four of these sit in the archive,
            // and without a row each would be re-decoded every pass for ever.
            out.unreadable += 1;
            ledger(ingest, REGISTER_SEGMENT, &filename, "unreadable", now)?;
            continue;
        };
        let end = start + Duration::microseconds((media.duration_s * 1e6).round() as i64);
        // ⚠ **`python_isoformat_utc`, NOT `SecondsFormat::Micros`, and the
        // difference is the idempotency key.** `UNIQUE (source_id, start_utc)`
        // compares TEXT. Every one of the 16,821 microphone rows already here
        // was written by the Python and spells a whole second WITHOUT a
        // fraction; `Micros` would write `...29.000000+00:00`, which is the
        // same instant, a different string, and therefore no conflict at all —
        // so a clip the `have` set missed would get a SECOND row rather than
        // being absorbed. Measured, not assumed: mic rows 16,821/16,821
        // whole-second, room rows 5,645/5,645 fractional.
        let inserted = meaning.execute(
            "INSERT OR IGNORE INTO audio_segments
                 (source_id, path, start_utc, end_utc, sample_rate, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                source,
                path.to_string_lossy(),
                crate::instant::python_isoformat_utc(start),
                crate::instant::python_isoformat_utc(end),
                media.sample_rate,
                media.channels,
            ],
        )?;
        // ⚠ **An IGNORED insert is a DECISION, not a no-op.** A sibling file
        // holds this minute — the `.wav` beside the `.opus` — so there is
        // nothing more this pass can do with the clip, and saying so is what
        // stops it being decoded again on the next one, and the one after.
        // Counted apart from `added` so the duplicate archive stays visible
        // rather than hiding inside a success total.
        if inserted == 1 {
            out.added += 1;
            // ⚠ THIS PASS's own work counts. `minutes` is a snapshot taken
            // before the loop, so without this a clip's sibling a few
            // candidates later is invisible and pays a full decode — inside the
            // very pass that just registered the minute. The test measures the
            // decode count, which is the only reason this was found rather than
            // reasoned past.
            minutes.insert(minute);
        } else {
            out.covered += 1;
        }
        let outcome = if inserted == 1 {
            "registered"
        } else {
            "covered-by-sibling"
        };
        ledger(ingest, REGISTER_SEGMENT, &filename, outcome, now)?;
    }
    Ok(out)
}

/// Every registered clip's BASENAME. One pass over the column rather than a
/// correlated lookup per candidate: the correlated form was a full scan of both
/// tables and ran ten minutes against the live fleet before it was killed.
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

/// Every registered `(source_id, start_utc)` — the MINUTE, not the filename,
/// and that difference is what makes a duplicate free.
///
/// ⚠ [`registered_names`] is keyed on basename, so the `.wav` beside the
/// `.opus` misses it and reaches the probe, which decodes the WHOLE FILE to
/// learn a duration the ignored insert then throws away. But a clip's start time
/// is in its NAME, and `(source_id, start_utc)` is the very key the `UNIQUE`
/// constraint rejects on — so the answer is knowable before any decoding
/// happens. 1,599 clips of this archive are such siblings.
fn registered_minutes(
    meaning: &rusqlite::Connection,
) -> rusqlite::Result<std::collections::HashSet<(String, String)>> {
    let mut stmt = meaning.prepare("SELECT source_id, start_utc FROM audio_segments")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    rows.collect()
}

/// Marks a per-mic turn hidden because a room turn now covers its minute.
///
/// A reason, not a flag: `hidden_reason` is what a reader sees when asking why a
/// turn vanished, and "the room stream covers this" is recoverable information
/// where a bare `1` is not.
pub const COVERED_BY_ROOM: &str = "covered by the room stream";

/// Marks a provisional LIVE turn hidden because the archive pass has reached it.
///
/// ⚠ The literal is shared with the Python (`store.RECONCILED_MARKER`) and with
/// `work::store_segment`. Three writers, one string, and a fourth spelling would
/// simply make some hidden turns unfindable by whoever goes looking for the
/// other three.
pub const LIVE_RECONCILED: &str = "live-reconciled";

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
    span: (DateTime<Utc>, DateTime<Utc>),
    plan: &Plan,
    stream: &Stream,
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
                // ⚠ The same rule `diarized` applies, on the OTHER stored
                // timing encoding — this path holds most of the archive's
                // turns, so leaving it out left the signal covering a tenth of
                // what it can see (#1410). `word_spans` reads both spellings.
                turn.word_timings
                    .as_deref()
                    .map_or(turn.confidence, |timings| {
                        if crate::quality::is_implausibly_slow(&crate::quality::word_spans(timings))
                        {
                            Some(0.0)
                        } else {
                            turn.confidence
                        }
                    }),
                stream.model,
                stream.provenance,
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
    if stream.reconciles_live {
        // ⚠ The SPAN, not the audio segment. A live turn has no
        // `audio_segment_id` of its own — it was minted from a stream, not a
        // file — so the only thing relating it to this clip is the minute it
        // fell in. Mirrors `work::store_segment`, which does this for the
        // sync-push path, down to the marker string.
        let (from, to) = span;
        tx.execute(
            "UPDATE transcript_segments SET hidden_reason = ?1
             WHERE asr_model = 'live' AND superseded_by IS NULL
               AND hidden_reason IS NULL
               AND start_utc >= ?2 AND start_utc < ?3",
            rusqlite::params![
                LIVE_RECONCILED,
                from.to_rfc3339_opts(SecondsFormat::Micros, false),
                to.to_rfc3339_opts(SecondsFormat::Micros, false),
            ],
        )?;
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

/// Which transcription stream a pass is draining, and the three things that
/// differ between them. Everything else in this module is shared.
///
/// ⚠ **A `Stream` is the unit of REVERSAL, which is why `provenance` is in it
/// rather than derived.** `DELETE FROM transcript_segments WHERE provenance =
/// '<stream>'` must name exactly the rows one pass wrote and no others — a
/// stream sharing a provenance string with another, or writing NULL like the
/// corpus convention does, is a stream nobody can take back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stream<'a> {
    /// The queue job kind whose stored results this pass interprets.
    pub kind: &'a str,
    /// What the written rows record in `transcript_segments.provenance` — the
    /// reversal key, unique per stream.
    pub provenance: &'a str,
    /// What they record in `asr_model`.
    pub model: &'a str,
    /// Whether writing turns for a clip also hides the PROVISIONAL LIVE turns
    /// standing on the same span.
    ///
    /// ⚠ **Not the same act as `hides_covered`, and not optional for a stream
    /// that replaces the archive pass.** A live turn is a guess made while
    /// somebody was still speaking; the archive turn for that span supersedes
    /// it, and `worker.py::reconcile_live` has been hiding them on the Mac for
    /// months. On the fleet the same thing happens in `work::store_segment`,
    /// which is the SYNC-PUSH path — and a runner writing turns directly never
    /// goes through it. Without this, the timeline shows the live guess and the
    /// archive turn side by side, which reads as the conversation happening
    /// twice.
    pub reconciles_live: bool,
    /// Whether a written turn HIDES the per-mic turns it covers.
    ///
    /// True for the room stream alone, and it is the whole reason [`plan`]'s
    /// rules 2, 3 and 4 exist. A per-mic pass writes turns for clips that have
    /// NONE; there is nothing standing on that minute for it to stand in for,
    /// and a pass that hid anything would be replacing a transcript rather than
    /// filling a gap.
    pub hides_covered: bool,
}

/// The derived one-microphone-per-minute stream (`transcribe-room`).
pub const ROOM: Stream<'static> = Stream {
    kind: crate::queue::TRANSCRIBE_ROOM,
    provenance: "room",
    model: ROOM_MODEL,
    // The room stream is DERIVED from microphones whose own archive pass already
    // reconciled the live turns on that minute; doing it again would hide the
    // same rows for a second reason and make the reversal ambiguous.
    reconciles_live: false,
    hides_covered: true,
};

/// What the `asr` shim loads when the caller names no model, spelled the way
/// `recall.asr.DEFAULT_MODEL` spells it.
///
/// ⚠ **One string, two languages, and the queue does not carry a model field.**
/// The shim is told nothing, so it uses its own default and this side has to
/// know what that is — which makes the two copies drift silently the day
/// somebody bumps the Python one. `a_per_mic_turn_names_the_model_the_shim_will
/// _actually_load` reads `asr.py` and fails on the mismatch; that test is the
/// only thing holding them together.
pub const SHIM_MODEL: &str = "mlx-community/whisper-large-v3-turbo";

/// One microphone's own clip (`transcribe-segment`) — `worker.py`'s loop, moved.
///
/// ⚠ `model` is the shim's real default, NOT a decorated name like [`ROOM`]'s:
/// these rows sit in the same per-microphone corpus that `worker.py` has been
/// writing for months, and a reader filtering on `asr_model` must not see the
/// archive split in two on the day the orchestrator changed. The provenance
/// field carries the "who wrote it" question instead, where a reader who is
/// asking it will look.
pub const PER_MIC: Stream<'static> = Stream {
    kind: crate::queue::TRANSCRIBE_SEGMENT,
    provenance: "per-mic (runner)",
    model: SHIM_MODEL,
    // This IS the archive pass now, so it inherits the archive pass's duty.
    reconciles_live: true,
    hides_covered: false,
};

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

/// The ledger of clips a pass DECIDED WITHOUT WRITING, in the ingest plane.
///
/// ⚠ Only refusals go here. A clip whose turns were written needs no row — the
/// turns are the record, and `write_pass` derives "already done" from them — so
/// deleting a stream's turns re-enables its clips by itself. A clip that wrote
/// NOTHING leaves no such trace, and without a row sits at the head of the
/// queue for ever.
///
/// ⚠ Therefore a reversal is TWO planes: the turns, and these rows.
///
/// # Errors
/// If the database refuses.
/// Whether the block starting at `block_start` on `source` was deliberately
/// deleted on the fleet — the same second-resolution match the audio lookup
/// uses, because the tombstone carries whatever spelling the row had.
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
            "SELECT 1 FROM deleted_segments WHERE source_id = ?1 AND start_utc LIKE ?2",
            rusqlite::params![
                source,
                format!("{}%", block_start.format("%Y-%m-%dT%H:%M:%S"))
            ],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

pub fn ensure_ledger(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pass_ledger (
             kind        TEXT NOT NULL,
             filename    TEXT NOT NULL,
             outcome     TEXT NOT NULL,
             decided_utc TEXT NOT NULL,
             PRIMARY KEY (kind, filename)
         );",
    )
}

/// A clip was decided and wrote nothing. `outcome` is for a person reading the
/// table later, never branched on.
///
/// ⚠ Keyed on (kind, filename), not filename. Three passes share this table —
/// the two turn streams and the segment registrar — and they reach the SAME
/// clip by the same name. A shared key would let one pass's refusal retire
/// another's work silently, with nothing anywhere saying so.
/// Record a terminal decision about a clip.
///
/// # Errors
/// If the database refuses.
pub fn ledger(
    conn: &rusqlite::Connection,
    kind: &str,
    filename: &str,
    outcome: &str,
    now: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pass_ledger (kind, filename, outcome, decided_utc)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![kind, filename, outcome, now],
    )?;
    Ok(())
}

/// Turn stored job results into visible turns, one clip at a time:
/// [`interpret`] → [`plan`] → [`write_block`].
///
/// ⚠ `limit` counts clips DECIDED, and the SQL has no `LIMIT`. A limited query
/// returns the same ineligible rows every pass, which is how an earlier version
/// re-examined the newest twenty blocks for ever and never advanced.
///
/// ⚠ ASCENDING, so a backfill drains forward from the oldest undecided clip.
///
/// # Errors
/// If either database refuses. An uninterpretable result is counted and
/// skipped — one bad clip must not stop the queue draining.
pub fn write_pass(
    meaning: &mut rusqlite::Connection,
    ingest: &rusqlite::Connection,
    stream: &Stream,
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
    //
    // ⚠ **The source is JOINED from the ingest plane, never parsed out of the
    // filename.** `<source>-<stamp>.<ext>` looks decomposable until a source is
    // itself hyphenated and stamped — `meeting-20260907-0905` is a real source
    // id here — and a split on the wrong hyphen would look up the audio segment
    // of a source that does not exist and silently find nothing. The ingest
    // plane already knows who uploaded each blob; ask it.
    let mut stmt = ingest.prepare(
        "SELECT j.filename, j.result, s.source FROM jobs j
         JOIN segments s ON s.filename = j.filename
         WHERE j.kind = ?1 AND j.done_utc IS NOT NULL AND j.result IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM pass_ledger l
                           WHERE l.kind = ?1 AND l.filename = j.filename)
         ORDER BY j.filename ASC",
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
        // The clip's own audio segment. Absent means nothing has registered it
        // in the meaning plane yet — a reason to wait, never to write a turn
        // with no audio. `audio_segment_id` is what `/api/audio/{id}` plays a
        // turn from, so a turn without one is text nobody can listen to.
        //
        // ⚠ **The one barren cause that gets NO ledger row.** It is the only
        // transient one, and a row here would retire a clip permanently for
        // being examined a few seconds too early.
        let Ok((audio_id, end_raw)) = meaning.query_row(
            "SELECT id, end_utc FROM audio_segments
             WHERE source_id = ?1 AND start_utc LIKE ?2",
            rusqlite::params![
                source,
                format!("{}%", block_start.format("%Y-%m-%dT%H:%M:%S"))
            ],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        ) else {
            pass.barren += 1;
            // ⚠ Unless the session was DELETED: then the audio is never coming,
            // and "wait" would be this pass re-examining the clip for ever
            // (#1653). The tombstone journal is what tells the two apart.
            if tombstoned_block(meaning, &source, block_start)? {
                ledger(ingest, stream.kind, &filename, "deleted", now)?;
            }
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
            ledger(ingest, stream.kind, &filename, "unreadable", now)?;
            continue;
        };
        // ⚠ **The clip's end comes from its own row, not from the room grid.**
        // A room block is exactly `BLOCK_S` because the builder cuts a UTC-aligned
        // grid; a microphone clip is whatever ffmpeg's segment muxer closed, and
        // capture stopping mid-segment makes short ones routinely. Asserting a
        // minute there would size the human-correction window wrong, and the
        // direction it errs is the one that matters: a window that ends early
        // cannot see a correction it is about to overwrite.
        let Ok(block_end) = DateTime::parse_from_rfc3339(&end_raw) else {
            pass.barren += 1;
            ledger(ingest, stream.kind, &filename, "unspanned", now)?;
            continue;
        };
        let block_end = block_end.with_timezone(&Utc);
        // Read only for the stream that can hide: this is a scan per clip, and
        // a per-mic pass that collected it would be paying for a list it is
        // structurally forbidden to act on.
        let standing = if stream.hides_covered {
            standing_between(meaning, block_start, block_end)?
        } else {
            Vec::new()
        };
        let human = corrected_between(meaning, block_start, block_end)?;
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
            // Decided, and left no trace in the meaning plane to derive that
            // from. Without this row the clip is indistinguishable from one
            // nobody has looked at, and every later pass reaches it first.
            ledger(ingest, stream.kind, &filename, "nothing-to-write", now)?;
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
