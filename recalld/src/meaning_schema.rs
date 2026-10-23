//! The meaning plane's schema: the ordered migration ladder for `recall.sqlite`.
//!
//! The integration test builds from empty and diffs against a dump of the live
//! fleet database.

#![expect(
    clippy::disallowed_methods,
    reason = "the schema itself: it runs on every open, so every test runs it"
)]

use rusqlite::Connection;

/// Entry i migrates version i to version i+1. Append only: editing a shipped
/// entry changes nothing on a database that already passed it.
#[expect(clippy::needless_raw_string_hashes, reason = "uniform by construction")]
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
    // v50
    r#"
    -- 1 on a person's turn whose words they typed or vouched for: where it was
    -- done does not matter, and a speaker fix (the machine's words, renamed)
    -- stays NULL. What Check skips.
    ALTER TABLE transcript_segments ADD COLUMN words_checked INTEGER;
    UPDATE transcript_segments SET words_checked = 1
     WHERE asr_model = 'human'
       AND EXISTS (SELECT 1 FROM corrections c
                   WHERE transcript_segments.provenance = 'human correction of #' || c.transcript_segment_id
                     AND (c.words_checked = 1 OR c.corrected_text <> c.original_text));
"#,
    // v51
    r#"
    -- The language a person pinned an uploaded session to ('nl', 'en'); NULL
    -- leaves it to the model's guess. Every transcription of its clips uses it.
    ALTER TABLE sources ADD COLUMN language TEXT;
"#,
    // v52
    r#"
    -- A clip's lines, hidden ones too: idx_ts_audio_current covers only the
    -- visible. The turn writer counts them per candidate, every round.
    CREATE INDEX idx_ts_audio ON transcript_segments(audio_segment_id);
"#,
    // v53
    r#"
    -- What people did to the record, in order (`transcript::Act` as JSON).
    -- clip_id / source_id: the clip or session the act touches (a retraction
    -- carries its target's), so one clip's acts load without parsing them all.
    -- origin: the row of the old tables an act was recovered from.
    CREATE TABLE edits (
        id        INTEGER PRIMARY KEY,
        at_us     INTEGER NOT NULL,
        act       TEXT NOT NULL CHECK (json_valid(act)),
        clip_id   INTEGER,
        source_id TEXT,
        origin    TEXT
    );
    CREATE INDEX edits_clip ON edits(clip_id) WHERE clip_id IS NOT NULL;
    CREATE INDEX edits_source ON edits(source_id) WHERE source_id IS NOT NULL;
    -- Append-only: a change of mind is a new act. Deleting a session deletes
    -- its acts.
    CREATE TRIGGER edits_append_only BEFORE UPDATE ON edits
    BEGIN SELECT RAISE(ABORT, 'edits: append-only'); END;
"#,
    // v54
    r#"
    -- The lines recall shows: render's output per clip, rebuilt when what the
    -- clip is made from changes. Shaped like transcript_segments' read columns.
    -- hidden_reason: model text render left out, and why. edit_id: the act that
    -- wrote or dropped the line. live_id: the live turn a live line shows, until
    -- a transcribed clip covers its time.
    CREATE TABLE rendered_lines (
        id               INTEGER PRIMARY KEY,
        clip_id          INTEGER,
        audio_segment_id INTEGER,
        source_id        TEXT,
        start_utc        TEXT NOT NULL,
        end_utc          TEXT NOT NULL,
        text             TEXT NOT NULL,
        language         TEXT,
        asr_confidence   REAL,
        asr_model        TEXT,
        speaker_label    TEXT,
        speaker_guess    TEXT,
        speaker_score    REAL,
        speaker_cluster  TEXT,
        tier             TEXT NOT NULL,
        hidden_reason    TEXT,
        words_checked    INTEGER,
        edit_id          INTEGER,
        live_id          INTEGER
    );
    CREATE INDEX rendered_clip ON rendered_lines(clip_id);
    CREATE INDEX rendered_live ON rendered_lines(live_id) WHERE live_id IS NOT NULL;
    CREATE INDEX rendered_audio ON rendered_lines(audio_segment_id);
    CREATE INDEX rendered_start ON rendered_lines(start_utc, id);
    CREATE INDEX rendered_source_start ON rendered_lines(source_id, start_utc, id);
    CREATE VIRTUAL TABLE rendered_fts USING fts5(text, content='rendered_lines', content_rowid='id');
    CREATE TRIGGER rendered_fts_insert AFTER INSERT ON rendered_lines BEGIN
        INSERT INTO rendered_fts (rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER rendered_fts_delete AFTER DELETE ON rendered_lines BEGIN
        INSERT INTO rendered_fts (rendered_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER rendered_fts_update AFTER UPDATE OF text ON rendered_lines BEGIN
        INSERT INTO rendered_fts (rendered_fts, rowid, text) VALUES ('delete', old.id, old.text);
        INSERT INTO rendered_fts (rowid, text) VALUES (new.id, new.text);
    END;
    -- What each clip's lines were last rendered from.
    CREATE TABLE rendered_clips (
        clip_id INTEGER PRIMARY KEY,
        inputs  TEXT NOT NULL
    );
"#,
    // v55
    r#"
    -- A model line's words: [[start_us, end_us, text], ...], their texts joined
    -- and trimmed being the line's. Where a person's span edge falls in it.
    ALTER TABLE rendered_lines ADD COLUMN word_timings TEXT;
"#,
    // v56
    r#"
    -- The naming act a voiceprint was learnt from (edits.id).
    ALTER TABLE speaker_embeddings ADD COLUMN source_edit_id INTEGER;
"#,
    // v57
    r#"
    -- The live feed's provisional lines, shown until a transcribed clip covers
    -- them (`view`). Moved out of transcript_segments with their ids, which
    -- rendered_lines.live_id names. created_utc: when it arrived, for the lag.
    CREATE TABLE live_lines (
        id          INTEGER PRIMARY KEY,
        start_utc   TEXT NOT NULL,
        end_utc     TEXT NOT NULL,
        text        TEXT NOT NULL,
        language    TEXT,
        asr_model   TEXT NOT NULL,
        created_utc TEXT
    );
    CREATE INDEX live_lines_start ON live_lines(start_utc, text);
    INSERT INTO live_lines (id, start_utc, end_utc, text, language, asr_model, created_utc)
        SELECT id, start_utc, end_utc, text, language, asr_model, created_utc
        FROM transcript_segments
        WHERE asr_model = 'live' AND superseded_by IS NULL AND hidden_reason IS NULL;
"#,
    // v58
    r#"
    -- AUTOINCREMENT: an id the app holds never comes back as another line.
    CREATE TABLE rendered_lines_v58 (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        clip_id          INTEGER,
        audio_segment_id INTEGER,
        source_id        TEXT,
        start_utc        TEXT NOT NULL,
        end_utc          TEXT NOT NULL,
        text             TEXT NOT NULL,
        language         TEXT,
        asr_confidence   REAL,
        asr_model        TEXT,
        speaker_label    TEXT,
        speaker_guess    TEXT,
        speaker_score    REAL,
        speaker_cluster  TEXT,
        tier             TEXT NOT NULL,
        hidden_reason    TEXT,
        words_checked    INTEGER,
        edit_id          INTEGER,
        live_id          INTEGER,
        word_timings     TEXT
    );
    INSERT INTO rendered_lines_v58
        SELECT id, clip_id, audio_segment_id, source_id, start_utc, end_utc, text,
               language, asr_confidence, asr_model, speaker_label, speaker_guess,
               speaker_score, speaker_cluster, tier, hidden_reason, words_checked,
               edit_id, live_id, word_timings
        FROM rendered_lines;
    DROP TABLE rendered_lines;
    ALTER TABLE rendered_lines_v58 RENAME TO rendered_lines;
    CREATE INDEX rendered_clip ON rendered_lines(clip_id);
    CREATE INDEX rendered_live ON rendered_lines(live_id) WHERE live_id IS NOT NULL;
    CREATE INDEX rendered_audio ON rendered_lines(audio_segment_id);
    CREATE INDEX rendered_start ON rendered_lines(start_utc, id);
    CREATE INDEX rendered_source_start ON rendered_lines(source_id, start_utc, id);
    CREATE TRIGGER rendered_fts_insert AFTER INSERT ON rendered_lines BEGIN
        INSERT INTO rendered_fts (rowid, text) VALUES (new.id, new.text);
    END;
    CREATE TRIGGER rendered_fts_delete AFTER DELETE ON rendered_lines BEGIN
        INSERT INTO rendered_fts (rendered_fts, rowid, text) VALUES ('delete', old.id, old.text);
    END;
    CREATE TRIGGER rendered_fts_update AFTER UPDATE OF text ON rendered_lines BEGIN
        INSERT INTO rendered_fts (rendered_fts, rowid, text) VALUES ('delete', old.id, old.text);
        INSERT INTO rendered_fts (rowid, text) VALUES (new.id, new.text);
    END;
"#,
    // v59
    r#"
    -- Clips transcribed before the jobs table: they have no job, so this is
    -- what keeps the queue from transcribing them again (`queue`).
    CREATE TABLE transcribed_before_jobs (
        audio_segment_id INTEGER PRIMARY KEY
    );
    INSERT INTO transcribed_before_jobs
        SELECT DISTINCT audio_segment_id FROM transcript_segments
        WHERE audio_segment_id IS NOT NULL;
    -- The tables the edit log and the rendered view replaced, dated and kept
    -- until a backup cycle has them. Nothing writes them but a session delete.
    DROP TRIGGER transcript_segments_instants_insert;
    DROP TRIGGER transcript_segments_instants_update;
    DROP TRIGGER corrections_instants_insert;
    DROP TRIGGER corrections_instants_update;
    DROP TRIGGER refine_requests_instants_insert;
    DROP TRIGGER refine_requests_instants_update;
    ALTER TABLE transcript_segments RENAME TO retired_20261012_transcript_segments;
    ALTER TABLE transcript_fts RENAME TO retired_20261012_transcript_fts;
    ALTER TABLE transcript_embeddings RENAME TO retired_20261012_transcript_embeddings;
    ALTER TABLE corrections RENAME TO retired_20261012_corrections;
    ALTER TABLE refine_requests RENAME TO retired_20261012_refine_requests;
"#,
    // v60
    r#"
    -- The retired tables are in the fleet backup; the record no longer keeps them.
    -- With foreign keys on, a drop deletes row by row and looks up each row's
    -- referrers: the children go first, and superseded_by, which refers to its
    -- own table, gets an index, or every row would scan the table.
    DROP TABLE retired_20261012_transcript_embeddings;
    DROP TABLE retired_20261012_corrections;
    CREATE INDEX retired_superseded_by ON retired_20261012_transcript_segments(superseded_by);
    DROP TABLE retired_20261012_transcript_segments;
    DROP TABLE retired_20261012_transcript_fts;
    DROP TABLE retired_20261012_refine_requests;
"#,
    // v61
    r#"
    -- Nothing reads these. language: a session's language is an edit act now.
    -- port, event_db, noise_shape: the Python capture's per-mic settings.
    ALTER TABLE sources DROP COLUMN port;
    ALTER TABLE sources DROP COLUMN event_db;
    ALTER TABLE sources DROP COLUMN noise_shape;
    ALTER TABLE sources DROP COLUMN language;
"#,
    // v62
    r#"
    -- Nothing reads these: the Python capture's per-clip measurements. The
    -- instant triggers guard two of them, so they are rebuilt without.
    DROP TRIGGER audio_segments_instants_insert;
    DROP TRIGGER audio_segments_instants_update;
    ALTER TABLE audio_segments DROP COLUMN transcribed_utc;
    ALTER TABLE audio_segments DROP COLUMN mean_volume;
    ALTER TABLE audio_segments DROP COLUMN envelope;
    ALTER TABLE audio_segments DROP COLUMN structure;
    ALTER TABLE audio_segments DROP COLUMN pushed_utc;
    ALTER TABLE audio_segments DROP COLUMN speech_s;
    CREATE TRIGGER audio_segments_instants_insert BEFORE INSERT ON audio_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'audio_segments: an instant not in the stored spelling'); END;
    CREATE TRIGGER audio_segments_instants_update BEFORE UPDATE OF start_utc, end_utc ON audio_segments
    WHEN NOT ((NEW.start_utc IS NULL OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.start_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00') AND
            (NEW.end_utc IS NULL OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.end_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'audio_segments: an instant not in the stored spelling'); END;
"#,
    // v63
    r#"
    -- Each source belongs to one person, a sign-in user id; NULL until
    -- claimed (`recalld claim-unowned`). A name is unique per owner, so two
    -- people may each know an "Alex". The index, not UNIQUE (owner, name):
    -- that treats NULL owners as distinct, and enrolment's INSERT OR IGNORE
    -- would make a second person of every unowned name.
    ALTER TABLE sources ADD COLUMN owner TEXT;
    -- Both tables are rebuilt, the prints with them: with foreign keys on, a
    -- parent dropped under its children counts every child as a violation,
    -- and a rename drags the children's REFERENCES along. Built beside the
    -- old pair, the old child dropped before the old parent, then renamed into
    -- place, which points the new prints at the new `speakers`.
    CREATE TABLE speakers_v63 (
        id    INTEGER PRIMARY KEY,
        owner TEXT,
        name  TEXT NOT NULL
    );
    INSERT INTO speakers_v63 (id, owner, name) SELECT id, NULL, name FROM speakers;
    CREATE TABLE speaker_embeddings_v63 (
        id                   INTEGER PRIMARY KEY,
        speaker_id           INTEGER NOT NULL REFERENCES speakers_v63(id),
        vector               TEXT NOT NULL,
        created_utc          TEXT NOT NULL,
        source_correction_id INTEGER,
        source_segment_id    INTEGER,
        source_edit_id       INTEGER
    );
    INSERT INTO speaker_embeddings_v63
        SELECT id, speaker_id, vector, created_utc, source_correction_id,
               source_segment_id, source_edit_id
        FROM speaker_embeddings;
    DROP TABLE speaker_embeddings;
    DROP TABLE speakers;
    ALTER TABLE speakers_v63 RENAME TO speakers;
    ALTER TABLE speaker_embeddings_v63 RENAME TO speaker_embeddings;
    CREATE UNIQUE INDEX speakers_owner_name ON speakers (ifnull(owner, ''), name);
    CREATE TRIGGER speaker_embeddings_instants_insert BEFORE INSERT ON speaker_embeddings
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'speaker_embeddings: an instant not in the stored spelling'); END;
    CREATE TRIGGER speaker_embeddings_instants_update BEFORE UPDATE OF created_utc ON speaker_embeddings
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'speaker_embeddings: an instant not in the stored spelling'); END;
"#,
    // v64
    r#"
    -- A glossary term belongs to one person, as a name does: it biases the
    -- transcription of that person's recordings only. Unique per owner by an
    -- index, so unowned terms still collide (see v63).
    CREATE TABLE vocabulary_v64 (
        id          INTEGER PRIMARY KEY,
        owner       TEXT,
        term        TEXT NOT NULL,
        created_utc TEXT NOT NULL
    );
    INSERT INTO vocabulary_v64 (id, owner, term, created_utc)
        SELECT id, NULL, term, created_utc FROM vocabulary;
    DROP TABLE vocabulary;
    ALTER TABLE vocabulary_v64 RENAME TO vocabulary;
    CREATE UNIQUE INDEX vocabulary_owner_term ON vocabulary (ifnull(owner, ''), term);
    CREATE TRIGGER vocabulary_instants_insert BEFORE INSERT ON vocabulary
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'vocabulary: an instant not in the stored spelling'); END;
    CREATE TRIGGER vocabulary_instants_update BEFORE UPDATE OF created_utc ON vocabulary
    WHEN NOT ((NEW.created_utc IS NULL OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9]+00:00' OR NEW.created_utc GLOB '[0-9][0-9][0-9][0-9]-[0-1][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-6][0-9].[0-9][0-9][0-9][0-9][0-9][0-9]+00:00'))
    BEGIN SELECT RAISE(ABORT, 'vocabulary: an instant not in the stored spelling'); END;
"#,
    // v65
    r#"
    -- Capture is off unless a person opens a recording window. A pause's
    -- resume-by, or blank for running, does not spell a window: every intent
    -- starts off, and the Mac's old pause report goes.
    UPDATE settings SET value = 'off'
        WHERE key = 'capture_intent' OR key LIKE 'capture_intent/%';
    DELETE FROM settings WHERE key = 'capture_reported_paused_until';
"#,
];

/// Bring `conn` up to the latest version, running only the steps it has not had.
///
/// # Errors
/// If a migration fails, or the database refuses the version pragma.
pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    ensure_to(conn, MIGRATIONS.len())
}

/// Bring `conn` up to version `want` and no further: a test's database as an
/// older release left it.
///
/// # Errors
/// As [`ensure`].
pub fn ensure_to(conn: &Connection, want: usize) -> rusqlite::Result<()> {
    let have: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let have = have as usize;
    if have >= want {
        return Ok(());
    }
    register_instant_utc(conn)?;
    // One transaction per step: a half-applied step must not be recorded, and a
    // step that succeeded must not be undone by a later one failing.
    for (index, statement) in MIGRATIONS.iter().enumerate().take(want).skip(have) {
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
