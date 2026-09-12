//! Stage E1 (docs/architecture.md): the work queue the Mac's runner polls.
//!
//! Jobs are DERIVED, not enqueued — the share-upload lesson: a
//! `transcribe-room` job exists for exactly every room segment without a
//! completed one, so a missed enqueue cannot strand audio. Leases are
//! time-bounded: a runner that dies mid-job simply lets its lease lapse and
//! the job is offered again. Newest room segment first — "what are they
//! saying now" outranks backfill (decision 8).

use crate::room::ROOM_SOURCE;
use crate::store;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

/// The job kind stage E starts with.
pub const TRANSCRIBE_ROOM: &str = "transcribe-room";
/// Stage E4: who spoke when, over a block whose words already exist. The
/// `voices` shim answers it (`recall.shim_voices`); ask/ab-compare follow.
pub const DIARIZE_ROOM: &str = "diarize-room";
/// One MICROPHONE's segment, transcribed by the same `asr` shim as a room block.
///
/// ⚠ **This is the orchestration port, not the room stream.** `worker.py` does
/// exactly this today — pick a segment, drive the model, write the turns — and
/// the runner already does that shape for room blocks. Same audio, same model,
/// same GPU; only the process holding the loop changes. So it carries none of
/// the room stream's open quality question, and does not wait on it.
///
/// ⚠ It does not fix throughput either. Measured 2026-09-12: 14,078 of 22,312
/// per-mic segments on Isis have no turns, so the Mac is 63% behind on its own
/// archive. The runner inherits that backlog at the same rate — one stream
/// instead of five (#1388) is the only thing that changes the arithmetic.
pub const TRANSCRIBE_SEGMENT: &str = "transcribe-segment";
const LEASE_TTL_S: i64 = 10 * 60;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    /// The blob to work on, fetchable via `/ingest/v1/blob/room/<filename>`.
    pub filename: String,
}

pub fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
             id           INTEGER PRIMARY KEY,
             kind         TEXT NOT NULL,
             filename     TEXT NOT NULL,
             state        TEXT NOT NULL DEFAULT 'queued',
             leased_until TEXT,
             attempts     INTEGER NOT NULL DEFAULT 0,
             created_utc  TEXT NOT NULL,
             done_utc     TEXT,
             result       TEXT,
             UNIQUE (kind, filename)
         );",
    )
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Derive queued jobs for room segments that have none. Idempotent; the
/// belt that makes a lost enqueue impossible.
pub fn derive_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    // ⚠ A segment MEASURED AS SILENT gets no job. Transcribing silence does not
    // return nothing — it returns INVENTIONS. Measured 2026-09-06 on the live
    // queue: a silent minute came back as "Thank you." twice, and another as 156
    // segments containing a 150-character run of tildes at 0.19 confidence
    // (#1410). 1784 of 4288 open jobs were silent, so this is 42% of the work
    // and most of the junk.
    //
    // Unmeasured segments still get one, matching the liveness rule: "not looked
    // at yet" is not evidence of silence, and on a host where the detector
    // cannot run (no AVX2-capable ONNX runtime) this degrades to the old
    // behaviour rather than silently producing no work at all.
    crate::speech::ensure_schema(conn)?;
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, s.filename, ?2 FROM segments s
         LEFT JOIN segment_speech p ON p.filename = s.filename
         WHERE s.source = ?3
           AND (p.filename IS NULL OR p.speech_seconds != 0.0)
           AND NOT EXISTS (SELECT 1 FROM jobs j
                           WHERE j.kind = ?1 AND j.filename = s.filename)",
        (TRANSCRIBE_ROOM, iso(now), ROOM_SOURCE),
    )?;
    Ok(inserted + derive_diarize_jobs(conn, now)?)
}

/// Derive a transcribe job for each microphone segment that has NO TURNS YET.
///
/// ⚠ **Two planes, and the join key is the FILENAME.** The segment rows live in
/// `ingest.sqlite` and the turns in `recall.sqlite`, and the obvious join —
/// `start_utc` to `start_utc` — silently matches NOTHING: the ingest plane
/// writes `2026-06-13T17:06:53Z` and the meaning plane
/// `2026-06-13T17:06:53+00:00`. Same instant, different spelling, compared as
/// TEXT. That cost a measurement here before it was noticed, because the answer
/// it returned was a confident zero rather than an error.
///
/// ⚠ **Bounded, and deliberately.** 14,078 segments were untranscribed when this
/// was written; deriving them all at once would queue days of GPU work in one
/// statement, competing with the room stream and with capture. `limit` is what
/// makes turning this on reversible.
///
/// # Errors
/// If either database refuses.
pub fn derive_segment_jobs(
    ingest: &Connection,
    meaning: &Connection,
    now: DateTime<Utc>,
    limit: usize,
) -> rusqlite::Result<usize> {
    // The silence table is this function's dependency too, not only
    // `derive_jobs`'s — transcribing a measured-silent clip returns INVENTIONS,
    // not nothing (#1410), and that rule is not room-specific.
    crate::speech::ensure_schema(ingest)?;
    // The filenames that already have turns, as basenames. Read from the meaning
    // plane in one pass rather than joined per row — a correlated LIKE over both
    // tables is a full scan of each, and on the live fleet it ran for ten
    // minutes before it was killed.
    let mut have: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let mut stmt = meaning.prepare(
            "SELECT DISTINCT a.path FROM audio_segments a
             JOIN transcript_segments t ON t.audio_segment_id = a.id
             WHERE a.source_id != ?1",
        )?;
        let rows = stmt.query_map([ROOM_SOURCE], |r| r.get::<_, String>(0))?;
        for path in rows {
            let path = path?;
            if let Some(name) = path.rsplit('/').next() {
                have.insert(name.to_owned());
            }
        }
    }

    let candidates: Vec<String> = {
        let mut stmt = ingest.prepare(
            "SELECT s.filename FROM segments s
             LEFT JOIN segment_speech p ON p.filename = s.filename
             WHERE s.source != ?1
               AND (p.filename IS NULL OR p.speech_seconds != 0.0)
               AND NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.kind = ?2 AND j.filename = s.filename)
             ORDER BY s.start_utc DESC",
        )?;
        let rows = stmt.query_map((ROOM_SOURCE, TRANSCRIBE_SEGMENT), |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };

    let mut inserted = 0;
    for filename in candidates {
        if inserted >= limit {
            break;
        }
        if have.contains(&filename) {
            continue;
        }
        inserted += ingest.execute(
            "INSERT OR IGNORE INTO jobs (kind, filename, created_utc) VALUES (?1, ?2, ?3)",
            (TRANSCRIBE_SEGMENT, &filename, iso(now)),
        )?;
    }
    Ok(inserted)
}

/// Derive a diarization job for every block whose transcription SUCCEEDED.
///
/// Gated on the words existing, for two reasons. Diarization alone attributes
/// nothing — it yields `SPEAKER_00` spans, and it is the alignment against words
/// that makes them turns (`refine.py`, which this replaces, transcribes first for
/// exactly this reason). And a block the ASR REFUSED is a block whose clip is the
/// problem (`room_turns::Barren::Refused`); handing the same clip to pyannote
/// spends GPU to learn that again.
///
/// Not gated on speech, because `transcribe-room` already is: a block with no
/// job never gets a done one, so silence cannot reach here.
fn derive_diarize_jobs(conn: &Connection, now: DateTime<Utc>) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT OR IGNORE INTO jobs (kind, filename, created_utc)
         SELECT ?1, j.filename, ?2 FROM jobs j
         WHERE j.kind = ?3 AND j.done_utc IS NOT NULL
           AND json_valid(j.result) AND json_extract(j.result, '$.ok') = 1
           AND NOT EXISTS (SELECT 1 FROM jobs d
                           WHERE d.kind = ?1 AND d.filename = j.filename)",
        (DIARIZE_ROOM, iso(now), TRANSCRIBE_ROOM),
    )
}

/// Lease the newest available job of a kind the caller can actually do:
/// queued, or leased-but-lapsed. The lease is the only mutation — a runner acks
/// by finishing, never by holding on.
///
/// ⚠ **`kinds` is what the RUNNER can do, not what exists.** A runner holds one
/// shim's weights, so a runner driving `asr` must never be handed a
/// `diarize-room`: it could only fail it, and the job would then cycle through
/// its attempts against a process that can never do it. An empty `kinds` leases
/// nothing, which is the safe reading of "I can do nothing".
pub fn lease(root: &Path, now: DateTime<Utc>, kinds: &[&str]) -> rusqlite::Result<Option<Job>> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    derive_jobs(&conn, now)?;
    // Built rather than bound as one parameter: SQLite has no array binding, and
    // the alternative (a comma-joined string matched with LIKE) would make
    // `transcribe-room` match a hypothetical `transcribe-room-v2`.
    // Numbered explicitly rather than bare `?`: SQLite allows mixing the two, but
    // the anonymous form's index is "one past the highest seen so far", which is
    // a rule nobody should have to remember while reading a WHERE clause.
    let places = (2..=kinds.len() + 1)
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, kind, filename FROM jobs
         WHERE state IN ('queued', 'leased')
           AND (leased_until IS NULL OR leased_until < ?1)
           AND done_utc IS NULL
           AND kind IN ({places})
         ORDER BY filename DESC LIMIT 1"
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(kinds.len() + 1);
    let stamp = iso(now);
    params.push(&stamp);
    for kind in kinds {
        params.push(kind);
    }
    let job: Option<Job> = conn
        .query_row(&sql, params.as_slice(), |r| {
            Ok(Job {
                id: r.get(0)?,
                kind: r.get(1)?,
                filename: r.get(2)?,
            })
        })
        .optional()?;
    if let Some(job) = &job {
        conn.execute(
            "UPDATE jobs SET state = 'leased', leased_until = ?1,
                             attempts = attempts + 1
             WHERE id = ?2",
            (iso(now + Duration::seconds(LEASE_TTL_S)), job.id),
        )?;
    }
    Ok(job)
}

/// Retire a job with its result (opaque JSON the E3 stage will interpret;
/// stored so nothing is lost while that lands).
pub fn done(root: &Path, id: i64, result: &str, now: DateTime<Utc>) -> rusqlite::Result<bool> {
    let conn = store::open(root)?;
    ensure_schema(&conn)?;
    let updated = conn.execute(
        "UPDATE jobs SET state = 'done', done_utc = ?1, result = ?2
         WHERE id = ?3 AND done_utc IS NULL",
        (iso(now), result, id),
    )?;
    Ok(updated == 1)
}
