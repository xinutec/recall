//! The meaning plane's schema: the ordered migration ladder for `recall.sqlite`.
//!
//! ⚠ **PORTED VERBATIM from `src/recall/store_schema.py`, which is DELETED.**
//! Every string below is that file's `_MIGRATIONS` tuple, entry for entry, emitted
//! mechanically rather than retyped. The Python was the only definition of the
//! schema behind 145,000 turns of archive, and nothing ran it: the fleet pod is
//! `recalld` alone, so the ladder had no runner and a fresh deployment could not
//! have created this database at all (#1538).
//!
//! ⚠ **APPEND ONLY. Never edit a shipped entry.** Each migrates the database from
//! version i to i+1, tracked in `PRAGMA user_version`, and runs exactly once.
//! Editing one changes nothing on any database that already passed it, so the
//! statement you wrote and the schema you have would silently disagree.
//!
//! The port is checked against the live fleet database rather than against
//! itself — see the integration test, which builds a database from this ladder
//! and diffs `sqlite_master` with a dump taken from Isis.

use rusqlite::Connection;

/// Entry i migrates version i to version i+1.
///
/// Every entry is a raw string with hashes, including the ones that do not need
/// them: the delimiter is uniform because these were emitted mechanically from
/// the Python, and picking a different quoting per entry is how an escape gets
/// wrong in a file where being wrong means a schema that differs from the
/// archive's.
#[allow(clippy::needless_raw_string_hashes, reason = "uniform by construction")]
pub const MIGRATIONS: &[&str] = &[
    // v1
    r#"
    CREATE TABLE sources (
        id   TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        kind TEXT NOT NULL
    );

    CREATE TABLE audio_segments (
        id          INTEGER PRIMARY KEY,
        source_id   TEXT NOT NULL REFERENCES sources(id),
        path        TEXT NOT NULL,
        start_utc   TEXT NOT NULL,
        end_utc     TEXT NOT NULL,
        sample_rate INTEGER NOT NULL,
        channels    INTEGER NOT NULL,
        UNIQUE (source_id, start_utc)
    );

    CREATE TABLE speakers (
        id   INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE
    );

    CREATE TABLE transcript_segments (
        id                  INTEGER PRIMARY KEY,
        audio_segment_id    INTEGER REFERENCES audio_segments(id),
        start_utc           TEXT NOT NULL,
        end_utc             TEXT NOT NULL,
        text                TEXT NOT NULL,
        language            TEXT,
        language_confidence REAL,
        asr_confidence      REAL,
        asr_model           TEXT NOT NULL,
        speaker_label       TEXT,
        speaker_id          INTEGER REFERENCES speakers(id),
        superseded_by       INTEGER REFERENCES transcript_segments(id)
    );

    CREATE INDEX idx_ts_start
        ON transcript_segments (start_utc) WHERE superseded_by IS NULL;

    CREATE VIRTUAL TABLE transcript_fts USING fts5(text, content='');

    -- Human corrections: the labelled (audio -> correct text) pairs that both
    -- fix the transcript and accumulate as the fine-tuning corpus.
    CREATE TABLE corrections (
        id                    INTEGER PRIMARY KEY,
        transcript_segment_id INTEGER REFERENCES transcript_segments(id),
        audio_segment_id      INTEGER REFERENCES audio_segments(id),
        start_utc             TEXT NOT NULL,
        end_utc               TEXT NOT NULL,
        original_text         TEXT NOT NULL,
        corrected_text        TEXT NOT NULL,
        language              TEXT,
        created_utc           TEXT NOT NULL
    );
"#,
    // v2
    r#"
    CREATE TABLE speaker_embeddings (
        id          INTEGER PRIMARY KEY,
        speaker_id  INTEGER NOT NULL REFERENCES speakers(id),
        vector      TEXT NOT NULL,
        created_utc TEXT NOT NULL
    );
"#,
    // v3
    r#"
    ALTER TABLE transcript_segments ADD COLUMN created_utc TEXT;
    ALTER TABLE transcript_segments ADD COLUMN provenance TEXT;

    CREATE TABLE transcript_lineage (
        derived_id INTEGER NOT NULL REFERENCES transcript_segments(id),
        source_id  INTEGER NOT NULL REFERENCES transcript_segments(id),
        PRIMARY KEY (derived_id, source_id)
    );
"#,
    // v4
    r#"
    ALTER TABLE audio_segments ADD COLUMN transcribed_utc TEXT;

    UPDATE audio_segments SET transcribed_utc = end_utc
    WHERE id IN (
        SELECT DISTINCT audio_segment_id FROM transcript_segments
        WHERE audio_segment_id IS NOT NULL
    );
"#,
    // v5
    r#"
    ALTER TABLE transcript_segments ADD COLUMN hidden_reason TEXT;
"#,
    // v6
    r#"
    ALTER TABLE corrections ADD COLUMN speaker TEXT;
"#,
    // v7
    r#"
    ALTER TABLE transcript_segments ADD COLUMN loudness REAL;
"#,
    // v8
    r#"
    ALTER TABLE speaker_embeddings ADD COLUMN source_correction_id INTEGER;
"#,
    // v9
    r#"
    ALTER TABLE corrections ADD COLUMN hidden_reason TEXT;
"#,
    // v10
    r#"
    ALTER TABLE transcript_segments ADD COLUMN speaker_guess TEXT;
    ALTER TABLE transcript_segments ADD COLUMN speaker_score REAL;
"#,
    // v11
    r#"
    CREATE TABLE transcript_embeddings (
        segment_id INTEGER PRIMARY KEY REFERENCES transcript_segments(id),
        vector     TEXT NOT NULL
    );
"#,
    // v12
    r#"
    ALTER TABLE corrections ADD COLUMN audio_confidence REAL;
"#,
    // v13
    r#"
    ALTER TABLE sources ADD COLUMN port INTEGER;
"#,
    // v14
    r#"
    ALTER TABLE transcript_segments ADD COLUMN speaker_cluster TEXT;
"#,
    // v15
    r#"
    ALTER TABLE transcript_segments ADD COLUMN word_timings TEXT;
"#,
    // v16
    r#"
    ALTER TABLE speaker_embeddings ADD COLUMN source_segment_id INTEGER;
"#,
    // v17
    r#"
    CREATE TABLE refine_requests (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id   TEXT NOT NULL REFERENCES sources(id),
        start_utc   TEXT NOT NULL,
        end_utc     TEXT NOT NULL,
        created_utc TEXT NOT NULL,
        done_utc    TEXT
    );
"#,
    // v18
    r#"
    CREATE TABLE ab_compare_runs (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id     TEXT NOT NULL REFERENCES sources(id),
        start_utc     TEXT,
        end_utc       TEXT,
        model_a       TEXT NOT NULL,
        model_b       TEXT NOT NULL,
        base_model    TEXT NOT NULL,
        status        TEXT NOT NULL,
        created_utc   TEXT NOT NULL,
        started_utc   TEXT,
        done_utc      TEXT,
        error         TEXT,
        result_json   TEXT,
        mean_wer_a    REAL,
        mean_wer_b    REAL,
        n_corrections INTEGER,
        n_segments    INTEGER,
        n_changed     INTEGER
    );
"#,
    // v19
    r#"
    UPDATE transcript_segments
    SET hidden_reason = 'live-reconciled', superseded_by = NULL
    WHERE asr_model = 'live' AND hidden_reason IS NULL AND superseded_by IN (
        SELECT id FROM transcript_segments WHERE asr_model != 'human'
    );
"#,
    // v20
    r#"
    UPDATE transcript_segments
    SET hidden_reason = 'live-reconciled', superseded_by = NULL
    WHERE asr_model = 'live' AND hidden_reason IS NULL AND superseded_by IN (
        SELECT id FROM transcript_segments WHERE asr_model != 'human'
    );
"#,
    // v21
    r#"
    UPDATE transcript_segments
    SET hidden_reason = COALESCE(hidden_reason, 'live-reconciled'),
        superseded_by = NULL
    WHERE asr_model = 'live' AND superseded_by IN (
        SELECT id FROM transcript_segments WHERE asr_model != 'human'
    );
"#,
    // v22
    r#"
    CREATE TABLE day_summaries (
        day         TEXT PRIMARY KEY,
        text        TEXT NOT NULL,
        model       TEXT NOT NULL,
        created_utc TEXT NOT NULL
    );
"#,
    // v23
    r#"
    CREATE TABLE vocabulary (
        id          INTEGER PRIMARY KEY,
        term        TEXT NOT NULL UNIQUE,
        created_utc TEXT NOT NULL
    );
"#,
    // v24
    r#"
    UPDATE corrections
    SET audio_confidence = (
        SELECT ts.asr_confidence FROM transcript_segments ts
        WHERE ts.id = corrections.transcript_segment_id
    )
    WHERE audio_confidence IS NULL AND transcript_segment_id IS NOT NULL;
"#,
    // v25
    r#"
    CREATE TABLE live_summaries (
        day           TEXT PRIMARY KEY,
        text          TEXT NOT NULL,
        model         TEXT NOT NULL,
        last_turn_id  INTEGER NOT NULL,
        generated_utc TEXT NOT NULL
    );
"#,
    // v26
    r#"
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
"#,
    // v27
    r#"
    DROP TABLE live_summaries;
    CREATE TABLE live_summaries (
        day           TEXT PRIMARY KEY,
        text          TEXT NOT NULL,
        model         TEXT NOT NULL,
        watermark     TEXT NOT NULL,
        generated_utc TEXT NOT NULL
    );
"#,
    // v28
    r#"
    ALTER TABLE audio_segments ADD COLUMN mean_volume REAL;
"#,
    // v29
    r#"
    CREATE INDEX idx_ts_current ON transcript_segments(id)
        WHERE superseded_by IS NULL AND hidden_reason IS NULL;
"#,
    // v30
    r#"
    CREATE INDEX idx_ts_audio_current ON transcript_segments(audio_segment_id)
        WHERE superseded_by IS NULL AND hidden_reason IS NULL;
"#,
    // v31
    r#"
    ALTER TABLE audio_segments ADD COLUMN envelope BLOB;
"#,
    // v32
    r#"
    ALTER TABLE sources ADD COLUMN event_db REAL;
"#,
    // v33
    r#"
    ALTER TABLE audio_segments ADD COLUMN speech_s REAL;
"#,
    // v34
    r#"
    ALTER TABLE audio_segments ADD COLUMN structure REAL;
"#,
    // v35
    r#"
    ALTER TABLE sources ADD COLUMN noise_shape BLOB;
"#,
    // v36
    r#"
    CREATE TABLE capture_events (
        id INTEGER PRIMARY KEY,
        utc TEXT NOT NULL,
        kind TEXT NOT NULL,
        source_id TEXT,
        detail TEXT
    );
    CREATE INDEX idx_capture_events_utc ON capture_events (utc);
"#,
    // v37
    r#"
    ALTER TABLE ab_compare_runs ADD COLUMN fleet_id INTEGER;
"#,
    // v38
    r#"
    ALTER TABLE audio_segments ADD COLUMN pushed_utc TEXT;

    CREATE TABLE deleted_segments (
        id          INTEGER PRIMARY KEY,
        source_id   TEXT NOT NULL,
        start_utc   TEXT NOT NULL,
        deleted_utc TEXT NOT NULL,
        swept_utc   TEXT,
        UNIQUE (source_id, start_utc)
    );
"#,
    // v39
    r#"
    CREATE TABLE sweep_refusals (
        id          INTEGER PRIMARY KEY,
        source_id   TEXT NOT NULL,
        start_utc   TEXT NOT NULL,
        refused_utc TEXT NOT NULL,
        reason      TEXT NOT NULL,
        UNIQUE (source_id, start_utc)
    );
"#,
    // v40
    r#"
    CREATE TABLE diarize_skips (
        audio_segment_id INTEGER PRIMARY KEY REFERENCES audio_segments(id),
        reason           TEXT NOT NULL,
        created_utc      TEXT NOT NULL
    );
"#,
    // v41
    r#"
    CREATE TABLE ask_requests (
        id          INTEGER PRIMARY KEY,
        fleet_id    INTEGER,
        question    TEXT NOT NULL,
        prompt      TEXT NOT NULL,
        sources     TEXT NOT NULL,
        created_utc TEXT NOT NULL,
        done_utc    TEXT,
        answer      TEXT,
        error       TEXT,
        UNIQUE (fleet_id)
    );
"#,
    // v42
    r#"
    CREATE TABLE unreadable_captures (
        source_id    TEXT NOT NULL,
        name         TEXT NOT NULL,
        recorded_utc TEXT NOT NULL,
        PRIMARY KEY (source_id, name)
    );
"#,
    // v43
    r#"
    DROP TABLE IF EXISTS sweep_refusals;
    ALTER TABLE deleted_segments DROP COLUMN swept_utc;
"#,
    // v44
    r#"
    DROP TABLE IF EXISTS ask_requests;
    DROP TABLE IF EXISTS ab_compare_runs;
    DROP TABLE IF EXISTS day_summaries;
    DROP TABLE IF EXISTS live_summaries;
"#,
    // v45
    r#"
    -- When this turn's speaker_guess was last derived. NULL means "before this
    -- column existed", which is exactly the population the re-match pass wants
    -- first: every guess written against a voiceprint corpus that has since
    -- grown (#1657).
    ALTER TABLE transcript_segments ADD COLUMN speaker_matched_utc TEXT;
"#,
];

/// Bring `conn` up to the latest version, running only the steps it has not had.
///
/// # Errors
/// If a migration fails, or the database refuses the version pragma.
pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    let have: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let want = MIGRATIONS.len() as u32;
    if have >= want {
        return Ok(());
    }
    for (index, statement) in MIGRATIONS.iter().enumerate().skip(have as usize) {
        // ⚠ One transaction PER STEP, not one for the ladder: a half-applied
        // step must not be recorded, and a step that succeeded must not be
        // undone by a later one failing. `user_version` moves with its own step.
        conn.execute_batch("BEGIN")?;
        conn.execute_batch(statement)?;
        conn.execute_batch(&format!("PRAGMA user_version = {}", index + 1))?;
        conn.execute_batch("COMMIT")?;
    }
    Ok(())
}
