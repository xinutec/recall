"""Database schema: the ordered migration ladder for the transcript store.

Split out of ``recall.store`` so the data-access logic isn't buried under ~230 lines
of DDL. ``recall.store`` imports ``_MIGRATIONS`` from here and derives
``SCHEMA_VERSION`` from its length; the runtime upgrade logic lives with the store.
"""

from __future__ import annotations

# Ordered schema migrations. Each entry migrates the database from version i to
# i+1 (tracked via PRAGMA user_version) and runs exactly once. Append new steps;
# never edit a shipped one. Real data depends on this.
_MIGRATIONS: tuple[str, ...] = (
    # v1 — initial schema
    """
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
    """,
    # v2 — speaker enrollment voiceprints (one row per reference embedding)
    """
    CREATE TABLE speaker_embeddings (
        id          INTEGER PRIMARY KEY,
        speaker_id  INTEGER NOT NULL REFERENCES speakers(id),
        vector      TEXT NOT NULL,
        created_utc TEXT NOT NULL
    );
    """,
    # v3 — data-preservation foundation for *structural* reprocessing.
    # provenance: how a version was derived (model/params/code) so every pass is
    #   reproducible and auditable. created_utc: when it was derived.
    # transcript_lineage: many-to-many supersession (e.g. 5 phrase-turns merged
    #   into 1 utterance) — the 1:1 superseded_by can't express that on its own.
    """
    ALTER TABLE transcript_segments ADD COLUMN created_utc TEXT;
    ALTER TABLE transcript_segments ADD COLUMN provenance TEXT;

    CREATE TABLE transcript_lineage (
        derived_id INTEGER NOT NULL REFERENCES transcript_segments(id),
        source_id  INTEGER NOT NULL REFERENCES transcript_segments(id),
        PRIMARY KEY (derived_id, source_id)
    );
    """,
    # v4 — mark which audio segments have been through ASR, separately from
    # whether they produced any transcript rows. A VAD-gated pass that finds no
    # speech writes zero rows but must not be retried forever; transcribed_utc
    # records "processed". Backfill: anything that already has transcripts is
    # processed, so the queue doesn't re-run the whole archive.
    """
    ALTER TABLE audio_segments ADD COLUMN transcribed_utc TEXT;

    UPDATE audio_segments SET transcribed_utc = end_utc
    WHERE id IN (
        SELECT DISTINCT audio_segment_id FROM transcript_segments
        WHERE audio_segment_id IS NOT NULL
    );
    """,
    # v5 — soft-hide: suppress a turn (e.g. a VAD-confirmed hallucination) without
    # deleting it. Hidden turns drop out of every current view but stay in the
    # table with a reason, so a hide is auditable and fully reversible.
    """
    ALTER TABLE transcript_segments ADD COLUMN hidden_reason TEXT;
    """,
    # v6 — who said it, on a human correction: the labeling UI tags each fragment
    # with a speaker, so the fine-tuning corpus is per-person (and seeds enrolment).
    """
    ALTER TABLE corrections ADD COLUMN speaker TEXT;
    """,
    # v7 — precomputed loudness (speech_level) per turn. Measuring it is a sox
    # decode; doing it per-candidate on the /api/train request path made the
    # labeling queue take ~13s for 80 candidates and time out the phone. Cache it
    # (filled offline by the worker) so the request is a cheap read + sort. NULL
    # means "not measured yet" → sorts last until the backfill reaches it.
    """
    ALTER TABLE transcript_segments ADD COLUMN loudness REAL;
    """,
    # v8 — voiceprints built from labelling. Every speaker-tagged correction is a
    # known (voice -> name) sample, so it doubles as enrolment: the backfill embeds
    # the clip and stores it as a reference voiceprint. source_correction_id links
    # each voiceprint to the tag it came from, so the backfill never re-embeds one
    # (NULL = a manually-recorded enrolment, kept for compatibility).
    """
    ALTER TABLE speaker_embeddings ADD COLUMN source_correction_id INTEGER;
    """,
    # v9 — soft-remove a bad label from the corpus (review/audit). A mis-labelled
    # correction is hidden, not deleted: it drops out of the training corpus, the
    # per-voice counts, and the review list, but the row is kept (recoverable).
    """
    ALTER TABLE corrections ADD COLUMN hidden_reason TEXT;
    """,
    # v10 — aggressive auto speaker guess. Every machine turn gets the best-
    # matching enrolled voice and its match strength (cosine), so the timeline can
    # show "Alice 31%" instead of "unknown". Kept separate from the human-
    # confirmed speaker_label (authoritative) and from speaker_id (strict resolve).
    """
    ALTER TABLE transcript_segments ADD COLUMN speaker_guess TEXT;
    ALTER TABLE transcript_segments ADD COLUMN speaker_score REAL;
    """,
    # v11 — store each turn's voiceprint vector once. Embedding (pyannote) is the
    # slow part; the match against enrolled voices is near-free. Persisting the
    # vector lets a guess be cheaply *re-matched* against current voiceprints as
    # labelling grows them — so guesses stay fresh without ever re-embedding (and
    # the timeline cache agrees with the live Train suggestion).
    """
    CREATE TABLE transcript_embeddings (
        segment_id INTEGER PRIMARY KEY REFERENCES transcript_segments(id),
        vector     TEXT NOT NULL
    );
    """,
    # v12 — audio confidence of the clip a correction was made on (the original
    # turn's asr_confidence). Lets the labelled corpus be weighted by audio quality
    # for ASR training, and lets voiceprint enrolment skip clips too degraded to
    # characterise a voice — the corrected text is still good ASR data, but a faint
    # or noisy embedding would poison the speaker print.
    """
    ALTER TABLE corrections ADD COLUMN audio_confidence REAL;
    """,
    # v13 — the TCP port a remote (phone) source streamed to, back when each device
    # had its own port. Single-port ingest identifies devices by handshake id now, so
    # this is vestigial; kept (the column is append-only). NULL for local sources.
    """
    ALTER TABLE sources ADD COLUMN port INTEGER;
    """,
    # v14 — the diarization cluster (e.g. 'SPEAKER_00') a refined turn was assigned to.
    # The relative voice within a recording, stored so the UI can group turns by voice
    # and let a person name a whole voice at once — distinct from speaker_guess (the
    # voiceprint match) and speaker_label (the human-confirmed name).
    """
    ALTER TABLE transcript_segments ADD COLUMN speaker_cluster TEXT;
    """,
    # v15 — per-word timings (JSON: [{s,e,w}, …], seconds relative to the turn start)
    # for diarized turns. The refine pass computes word-level alignment but discarded
    # it; persisting it lets a boundary edit (split/reassign) snap to a real word time
    # and play exactly that span, instead of interpolating by character position. NULL
    # for older turns (they fall back to interpolation until re-derived).
    """
    ALTER TABLE transcript_segments ADD COLUMN word_timings TEXT;
    """,
    # v16 — voiceprints sourced from any current human-labelled turn, not only text
    # corrections. source_segment_id links a reference embedding to the turn whose
    # speaker_label it was built from, so session-view speaker assigns (set_turn_speaker
    # / assign_span) teach the voices too — and a re-assignment or re-split lets the
    # stale print be pruned and re-derived. NULL for legacy correction-sourced rows.
    """
    ALTER TABLE speaker_embeddings ADD COLUMN source_segment_id INTEGER;
    """,
    # v17 — a queue of on-demand refine requests. The web UI can ask to diarize-refine
    # a chosen stretch of a recording (a conversation); the idle-gated refine daemon
    # picks pending rows up and re-derives those segments, so a heavy pass is still kept
    # off live capture. done_utc NULL = pending.
    """
    CREATE TABLE refine_requests (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        source_id   TEXT NOT NULL REFERENCES sources(id),
        start_utc   TEXT NOT NULL,
        end_utc     TEXT NOT NULL,
        created_utc TEXT NOT NULL,
        done_utc    TEXT
    );
    """,
    # v18 — a queue + history of A/B model comparison runs. The web UI enqueues a run
    # (compare two ASR models over a recording, non-destructively); a runner drains
    # queued rows, transcribes with both models, and writes the result back here. status
    # is queued -> running -> done|error; result_json holds the abcompare report and the
    # mean_wer_* / n_* columns are denormalized from it so the list view needs no parse.
    # start_utc/end_utc NULL = the whole recording.
    """
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
    """,
    # v19 — data repair. reconcile_live used to point every caught-up live turn's
    # superseded_by at ONE arbitrary archive turn, so deep links resolved to an
    # unrelated utterance. Rewrite those to hidden ('live-reconciled', the literal of
    # store.RECONCILED_MARKER). Live turns superseded by a HUMAN turn are genuine
    # corrections and stay as they are.
    """
    UPDATE transcript_segments
    SET hidden_reason = 'live-reconciled', superseded_by = NULL
    WHERE asr_model = 'live' AND hidden_reason IS NULL AND superseded_by IN (
        SELECT id FROM transcript_segments WHERE asr_model != 'human'
    );
    """,
    # v20 — the same repair once more: workers still running the pre-fix code kept
    # writing corrupt supersessions in the window between v19 applying and their
    # restart. Idempotent by construction.
    """
    UPDATE transcript_segments
    SET hidden_reason = 'live-reconciled', superseded_by = NULL
    WHERE asr_model = 'live' AND hidden_reason IS NULL AND superseded_by IN (
        SELECT id FROM transcript_segments WHERE asr_model != 'human'
    );
    """,
    # v21 — v19/v20 missed one variant: the old worker re-superseded live turns v19
    # had ALREADY hidden (its filter ignored hidden_reason), leaving rows both
    # hidden and corruptly superseded. Clear the pointer wherever it aims at a
    # non-human turn, keeping (or setting) the hidden marker.
    """
    UPDATE transcript_segments
    SET hidden_reason = COALESCE(hidden_reason, 'live-reconciled'),
        superseded_by = NULL
    WHERE asr_model = 'live' AND superseded_by IN (
        SELECT id FROM transcript_segments WHERE asr_model != 'human'
    );
    """,
    # v22 — the recall layer's per-day summaries. Derived views like transcripts:
    # regenerable, overwritten on re-derive (day is the key), each carrying the
    # model that wrote it.
    """
    CREATE TABLE day_summaries (
        day         TEXT PRIMARY KEY,
        text        TEXT NOT NULL,
        model       TEXT NOT NULL,
        created_utc TEXT NOT NULL
    );
    """,
    # v23 — the household vocabulary: proper nouns and terms the ASR should be
    # biased toward (names, places, medical terms). Feeds Whisper's initial_prompt
    # (recall.vocabulary) — the cheap proper-noun lever, no training involved.
    """
    CREATE TABLE vocabulary (
        id          INTEGER PRIMARY KEY,
        term        TEXT NOT NULL UNIQUE,
        created_utc TEXT NOT NULL
    );
    """,
    # v24 — data repair: corrections made before v12 predate audio_confidence;
    # backfill it from the corrected turn's own asr_confidence so quality
    # weighting covers the whole corpus.
    """
    UPDATE corrections
    SET audio_confidence = (
        SELECT ts.asr_confidence FROM transcript_segments ts
        WHERE ts.id = corrections.transcript_segment_id
    )
    WHERE audio_confidence IS NULL AND transcript_segment_id IS NOT NULL;
    """,
    # v25 — the "today so far" summary: a one-row cache of the running day's
    # provisional summary, stamped with the newest turn id it saw (the watermark)
    # so a request can tell fresh from stale without generating. Unlike
    # day_summaries (settled, one per finished day) this is always provisional
    # and evicted when the day rolls over.
    """
    CREATE TABLE live_summaries (
        day           TEXT PRIMARY KEY,
        text          TEXT NOT NULL,
        model         TEXT NOT NULL,
        last_turn_id  INTEGER NOT NULL,
        generated_utc TEXT NOT NULL
    );
    """,
    # v26 — free-form settings. First use: household_context, the background
    # facts prepended to the LLM prompts (summaries/ask). Personal facts are
    # DATA here, never hardcoded in the repo (which stays PII-free).
    """
    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    """,
    # v27 — the live-summary watermark becomes a day-state fingerprint (TEXT),
    # not a max turn id: a speaker-label edit changes rows in place, so the old
    # integer watermark never moved and human annotation could not reach the
    # today-summary. The table is a transient cache — dropping it just costs one
    # regeneration on the next look.
    """
    DROP TABLE live_summaries;
    CREATE TABLE live_summaries (
        day           TEXT PRIMARY KEY,
        text          TEXT NOT NULL,
        model         TEXT NOT NULL,
        watermark     TEXT NOT NULL,
        generated_utc TEXT NOT NULL
    );
    """,
    # v28 — cache each capture segment's RAW mean volume (dBFS) so the quiet-cleanup
    # scan measures a file once (ffmpeg is slow over the whole archive), then finds long
    # quiet spans from the cached values instantly. NULL = not yet measured.
    """
    ALTER TABLE audio_segments ADD COLUMN mean_volume REAL;
    """,
    # v29 — index the *current* transcript segments. Counting them (the status
    # endpoint) matched no index, so SQLite walked all 44k rows to test the two
    # NULL predicates — 8.7s cold on the archive, and >25s while the worker was
    # competing for the disk, which hung /api/status. A partial index holds only
    # the current rows, so the count reads it instead of the table.
    """
    CREATE INDEX idx_ts_current ON transcript_segments(id)
        WHERE superseded_by IS NULL AND hidden_reason IS NULL;
    """,
    # v30 — index current transcript segments BY THEIR AUDIO. Quiet-span detection asks,
    # per capture segment, "does a turn that still stands hang off this audio?" — the
    # veto that stops a delete from destroying a transcript. Nothing indexed
    # audio_segment_id, so each of the 9k segments full-scanned all 44k turns (~410M row
    # visits): /api/quiet/spans took 83s. Partial, mirroring idx_ts_current, because the
    # question is only ever about current, visible turns.
    """
    CREATE INDEX idx_ts_audio_current ON transcript_segments(audio_segment_id)
        WHERE superseded_by IS NULL AND hidden_reason IS NULL;
    """,
)
