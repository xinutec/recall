//! The ingest plane's schema (`ingest.sqlite`). Every open runs it. It only
//! creates what is missing: a change to an existing table needs a migration
//! of its own.

#![expect(
    clippy::disallowed_methods,
    reason = "the schema itself: it runs on every open, so every test runs it"
)]

use rusqlite::Connection;

pub fn ensure(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS segments (
             filename     TEXT PRIMARY KEY,
             source       TEXT NOT NULL,
             start_utc    TEXT NOT NULL,
             bytes        INTEGER NOT NULL,
             sha256       TEXT NOT NULL,
             received_utc TEXT NOT NULL,
             sent_utc     TEXT
         );
         CREATE INDEX IF NOT EXISTS segments_source_start
             ON segments (source, start_utc);

         CREATE TABLE IF NOT EXISTS segment_speech (
             filename       TEXT PRIMARY KEY REFERENCES segments (filename),
             source         TEXT NOT NULL,
             speech_seconds REAL NOT NULL,
             computed_utc   TEXT NOT NULL,
             -- Where the speech is, as JSON [[start, end], ...] in seconds from
             -- the blob's start; JSON null when it could not be decoded. NULL:
             -- not looked for yet (speech::scan_once fills it).
             regions        TEXT
         );
         CREATE INDEX IF NOT EXISTS segment_speech_source
             ON segment_speech (source, filename);

         CREATE TABLE IF NOT EXISTS segment_levels (
             filename     TEXT PRIMARY KEY REFERENCES segments (filename),
             source       TEXT NOT NULL,
             speech_db    REAL NOT NULL,
             floor_db     REAL NOT NULL,
             computed_utc TEXT NOT NULL,
             -- NULL means measured before the column existed: unknown, never zero.
             gated        REAL,
             quiet_run_s  REAL
         );
         CREATE INDEX IF NOT EXISTS segment_levels_source
             ON segment_levels (source, filename);

         CREATE TABLE IF NOT EXISTS jobs (
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
         );
         -- The open jobs, the few a lease looks at: a scan of all of them
         -- reads every result's first page.
         CREATE INDEX IF NOT EXISTS jobs_open ON jobs (kind) WHERE done_utc IS NULL;

         -- A pass's terminal decision per clip (ledger). detail: older
         -- decisions' counts, as JSON.
         CREATE TABLE IF NOT EXISTS pass_ledger (
             kind        TEXT NOT NULL,
             filename    TEXT NOT NULL,
             outcome     TEXT NOT NULL,
             decided_utc TEXT NOT NULL,
             detail      TEXT,
             PRIMARY KEY (kind, filename)
         );

         -- One row per stored recording, keyed by file (transcript::Clip).
         -- path is relative to the data root.
         CREATE TABLE IF NOT EXISTS clips (
             id       INTEGER PRIMARY KEY,
             source   TEXT NOT NULL,
             start_us INTEGER NOT NULL,
             filename TEXT NOT NULL UNIQUE,
             path     TEXT NOT NULL UNIQUE
         );
         CREATE INDEX IF NOT EXISTS clips_source_start ON clips (source, start_us);

         -- Every stored file has a clip, whoever stores it. A start that will
         -- not convert fails the file's insert. Starts are whole seconds, so
         -- strftime('%s') is exact. ON CONFLICT, not OR IGNORE: OR IGNORE
         -- would also skip a NOT NULL failure and drop the clip silently.
         CREATE TRIGGER IF NOT EXISTS segments_have_clips AFTER INSERT ON segments
         BEGIN
             INSERT INTO clips (source, start_us, filename, path)
             VALUES (NEW.source,
                     CAST(strftime('%s', NEW.start_utc) AS INTEGER) * 1000000,
                     NEW.filename,
                     'ingest/' || NEW.source || '/' || NEW.filename)
             ON CONFLICT (filename) DO NOTHING;
         END;",
    )
}
