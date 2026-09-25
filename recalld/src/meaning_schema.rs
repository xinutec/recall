//! The meaning plane's schema: the ordered migration ladder for `recall.sqlite`.
//!
//! The integration test builds from empty and diffs against a dump of the live
//! fleet database.

use rusqlite::Connection;

/// Entry i migrates version i to version i+1. APPEND ONLY — editing a shipped
/// entry changes nothing on a database that already passed it.
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
    // v46
    r#"
    -- One spelling for every stored instant: `instant_utc`, registered by
    -- `ensure`. The keys (`audio_segments`, `deleted_segments`) are compared as
    -- TEXT, so before this a room row's `...:00.000000+00:00` and a mic row's
    -- `...:00+00:00` needed a LIKE on the second to find each other.
    UPDATE audio_segments SET start_utc = instant_utc(start_utc), end_utc = instant_utc(end_utc), transcribed_utc = instant_utc(transcribed_utc), pushed_utc = instant_utc(pushed_utc);
    UPDATE transcript_segments SET start_utc = instant_utc(start_utc), end_utc = instant_utc(end_utc), created_utc = instant_utc(created_utc), speaker_matched_utc = instant_utc(speaker_matched_utc);
    UPDATE corrections SET start_utc = instant_utc(start_utc), end_utc = instant_utc(end_utc), created_utc = instant_utc(created_utc);
    UPDATE speaker_embeddings SET created_utc = instant_utc(created_utc);
    UPDATE refine_requests SET start_utc = instant_utc(start_utc), end_utc = instant_utc(end_utc), created_utc = instant_utc(created_utc), done_utc = instant_utc(done_utc);
    UPDATE vocabulary SET created_utc = instant_utc(created_utc);
    UPDATE capture_events SET utc = instant_utc(utc);
    UPDATE deleted_segments SET start_utc = instant_utc(start_utc), deleted_utc = instant_utc(deleted_utc);
    UPDATE diarize_skips SET created_utc = instant_utc(created_utc);
    UPDATE unreadable_captures SET recorded_utc = instant_utc(recorded_utc);
"#,
    // v47
    r#"
    -- Every stored instant in the one spelling (`audiocore::instant`): UTC,
    -- `+00:00`, no fraction or six digits. Compared as text, any other spelling
    -- misorders silently; refused here, it fails the write that made it.
    CREATE TRIGGER audio_segments_instants_insert BEFORE INSERT ON audio_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.transcribed_utc IS NULL OR NEW.transcribed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.transcribed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.pushed_utc IS NULL OR NEW.pushed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.pushed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'audio_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER audio_segments_instants_update BEFORE UPDATE OF start_utc, end_utc, transcribed_utc, pushed_utc ON audio_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.transcribed_utc IS NULL OR NEW.transcribed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.transcribed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.pushed_utc IS NULL OR NEW.pushed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.pushed_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'audio_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER transcript_segments_instants_insert BEFORE INSERT ON transcript_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.speaker_matched_utc IS NULL OR NEW.speaker_matched_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.speaker_matched_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'transcript_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER transcript_segments_instants_update BEFORE UPDATE OF start_utc, end_utc, created_utc, speaker_matched_utc ON transcript_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.speaker_matched_utc IS NULL OR NEW.speaker_matched_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.speaker_matched_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'transcript_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER corrections_instants_insert BEFORE INSERT ON corrections
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'corrections: an instant not in the stored spelling'); END;
    CREATE TRIGGER corrections_instants_update BEFORE UPDATE OF start_utc, end_utc, created_utc ON corrections
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'corrections: an instant not in the stored spelling'); END;
    CREATE TRIGGER speaker_embeddings_instants_insert BEFORE INSERT ON speaker_embeddings
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'speaker_embeddings: an instant not in the stored spelling'); END;
    CREATE TRIGGER speaker_embeddings_instants_update BEFORE UPDATE OF created_utc ON speaker_embeddings
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'speaker_embeddings: an instant not in the stored spelling'); END;
    CREATE TRIGGER refine_requests_instants_insert BEFORE INSERT ON refine_requests
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.done_utc IS NULL OR NEW.done_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.done_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'refine_requests: an instant not in the stored spelling'); END;
    CREATE TRIGGER refine_requests_instants_update BEFORE UPDATE OF start_utc, end_utc, created_utc, done_utc ON refine_requests
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.done_utc IS NULL OR NEW.done_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.done_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'refine_requests: an instant not in the stored spelling'); END;
    CREATE TRIGGER vocabulary_instants_insert BEFORE INSERT ON vocabulary
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'vocabulary: an instant not in the stored spelling'); END;
    CREATE TRIGGER vocabulary_instants_update BEFORE UPDATE OF created_utc ON vocabulary
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'vocabulary: an instant not in the stored spelling'); END;
    CREATE TRIGGER capture_events_instants_insert BEFORE INSERT ON capture_events
    WHEN NOT ((NEW.utc IS NULL OR NEW.utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'capture_events: an instant not in the stored spelling'); END;
    CREATE TRIGGER capture_events_instants_update BEFORE UPDATE OF utc ON capture_events
    WHEN NOT ((NEW.utc IS NULL OR NEW.utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'capture_events: an instant not in the stored spelling'); END;
    CREATE TRIGGER deleted_segments_instants_insert BEFORE INSERT ON deleted_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.deleted_utc IS NULL OR NEW.deleted_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.deleted_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'deleted_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER deleted_segments_instants_update BEFORE UPDATE OF start_utc, deleted_utc ON deleted_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.deleted_utc IS NULL OR NEW.deleted_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.deleted_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'deleted_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER diarize_skips_instants_insert BEFORE INSERT ON diarize_skips
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'diarize_skips: an instant not in the stored spelling'); END;
    CREATE TRIGGER diarize_skips_instants_update BEFORE UPDATE OF created_utc ON diarize_skips
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'diarize_skips: an instant not in the stored spelling'); END;
    CREATE TRIGGER unreadable_captures_instants_insert BEFORE INSERT ON unreadable_captures
    WHEN NOT ((NEW.recorded_utc IS NULL OR NEW.recorded_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.recorded_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'unreadable_captures: an instant not in the stored spelling'); END;
    CREATE TRIGGER unreadable_captures_instants_update BEFORE UPDATE OF recorded_utc ON unreadable_captures
    WHEN NOT ((NEW.recorded_utc IS NULL OR NEW.recorded_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.recorded_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'unreadable_captures: an instant not in the stored spelling'); END;
    "#,
    // v48
    r#"
    -- 1: a person listened and vouches for the words, changed or not. NULL: not
    -- said, as for a speaker fix, whose text is the machine's.
    ALTER TABLE corrections ADD COLUMN words_checked INTEGER;
"#,
    // v49
    r#"
    -- Left from the Python archive: nothing has written them since the port,
    -- and production held no rows.
    DROP TABLE transcript_lineage;
    DROP TABLE diarize_skips;
    DROP TABLE unreadable_captures;
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
    register_instant_utc(conn)?;
    // One transaction per step: a half-applied step must not be recorded, and a
    // step that succeeded must not be undone by a later one failing.
    for (index, statement) in MIGRATIONS.iter().enumerate().skip(have as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(statement)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", index + 1))?;
        tx.commit()?;
    }
    Ok(())
}

/// `instant_utc(text)`: the stored instant in [`audiocore::instant::python_isoformat_utc`]'s
/// spelling, NULL for NULL. A value that is not an instant fails the statement
/// rather than being kept or guessed at.
fn register_instant_utc(conn: &Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "instant_utc",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let Some(raw) = ctx.get::<Option<String>>(0)? else {
                return Ok(None);
            };
            audiocore::instant::parse_utc(&raw)
                .map(|t| Some(audiocore::instant::python_isoformat_utc(t)))
                .ok_or_else(|| {
                    rusqlite::Error::UserFunctionError(format!("not an instant: {raw}").into())
                })
        },
    )
}
