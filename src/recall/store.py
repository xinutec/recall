"""Searchable, versioned transcript store (SQLite + FTS5).

The store is the backbone of the memory aid. It holds:

- `sources` / `audio_segments` — the retained raw-audio index (source of truth).
- `transcript_segments` — derived, versioned views: each carries the model and
  confidences that produced it, and is *superseded* (never deleted) when a
  better pass replaces it. This is the "never commit, always re-derivable" model
  from pipeline.md §6.
- `transcript_fts` — FTS5 full-text index over transcript text.

Search and time-range queries return only *current* (non-superseded) segments.
"""

from __future__ import annotations

import hashlib
import json
import sqlite3
from collections.abc import Collection, Iterator, Sequence
from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Self

from recall.asr import Word
from recall.capture_control import CaptureEventKind
from recall.ids import AudioSegmentId, CorrectionId, SpeakerId, TranscriptId
from recall.ranking import normalize_text
from recall.sources import AudioSource, SourceKind, SourceRow
from recall.store_models import (
    AbCompareJob,
    CaptureEvent,
    Correction,
    LabelledFragment,
    LiveSummary,
    PendingVoiceprint,
    RefineRequest,
    SegmentVolume,
    SessionSummary,
    SourceCoverage,
    TranscriptSegment,
    UploadJob,
    VocabularyTerm,
)
from recall.store_schema import _MIGRATIONS
from recall.timeline import Segment

# Public API of the store package. Listed explicitly so the value types and schema
# now defined in store_models / store_schema are re-exported from `recall.store`
# (mypy --strict's no-implicit-reexport): `from recall.store import TranscriptSegment`
# (etc.) keeps working as before the split.
__all__ = [
    "ALIGNED_MARKER",
    "DIARIZED_MARKER",
    "HOUSEHOLD_LANGUAGES",
    "HUMAN_MODEL",
    "LIVE_MODEL",
    "REPROCESSED_MARKER",
    "SCHEMA_VERSION",
    "_MIGRATIONS",
    "AbCompareJob",
    "CaptureEvent",
    "Correction",
    "LabelledFragment",
    "LiveSummary",
    "PendingVoiceprint",
    "RefineRequest",
    "SegmentVolume",
    "SessionSummary",
    "SourceCoverage",
    "Store",
    "TranscriptSegment",
    "UploadJob",
    "VocabularyTerm",
]

# asr_model marker for human-authored (ground-truth) transcript segments.
# Reprocessing must never supersede these.
HUMAN_MODEL = "human"
# Turns shorter than this can't be embedded for speaker ID — pyannote's front-end
# conv needs more than a few samples, and a degenerate clip (near-zero or even
# negative span) crashes it. Such turns are skipped by the voiceprint work-list.
_MIN_GUESS_DURATION_S = 0.2
# A reference *voiceprint* needs more than a guess-embedding: ~a second of speech to
# characterise a voice. A sub-second sliver (e.g. a one-word split) or a near-silent
# clip would enrol a useless/misleading print, so they're gated out of enrolment — the
# turn's text and display label are unaffected.
_MIN_VOICEPRINT_SECONDS = 1.0
_MIN_VOICEPRINT_LOUDNESS = 0.01

# asr_model marker for the fast, provisional live-transcription pass. These get
# hidden (RECONCILED_MARKER) once the higher-quality archive transcription
# catches up to their time.
LIVE_MODEL = "live"

# The languages the household actually speaks. A whole-segment transcription that
# comes out as anything else (Japanese, Spanish, German…) is almost always the model
# hallucinating on unclear/far-field audio, not real speech — a strong unreliability
# signal. Single source of truth so capture/eval agree.
HOUSEHOLD_LANGUAGES = frozenset({"nl", "en"})


# Hidden-reason / provenance prefixes marking a turn produced by a re-derive pass.
# The resumable queries (_segments_without_marker), the writers (refine.py /
# redrive.py) and the UI tier check (api._tier) all key off these prefixes, so
# they live here as the single source of truth: a typo in one copy would silently
# break classification and make the pass re-run forever.
DIARIZED_MARKER = "diarized"
# Provenance of the *current* diarized pipeline (transcribe-then-align). Still starts
# with DIARIZED_MARKER (so it reads as the "diarized" tier), but distinguishes turns
# made by this pipeline from older diarized ones — which lets re-diarification find
# and upgrade the older ones, and terminate once everything is current.
ALIGNED_MARKER = "diarized-aligned"
REPROCESSED_MARKER = "reprocessed"
# Hidden-reason for live turns the archive has caught up to (worker.reconcile_live).
RECONCILED_MARKER = "live-reconciled"


def human_correction_provenance(original_id: int) -> str:
    """Provenance stamped on the human turn that replaces `original_id`.

    Written by review.apply_correction and MATCHED by set_correction_speaker /
    nudge_correction to find that live turn again — one function, so the writer
    and the matchers can never drift apart.
    """
    return f"human correction of #{original_id}"


SCHEMA_VERSION = len(_MIGRATIONS)


def _require_aware(value: datetime, field: str) -> None:
    if value.tzinfo is None or value.utcoffset() is None:
        msg = f"{field} must be timezone-aware"
        raise ValueError(msg)


class Store:
    """SQLite-backed transcript store."""

    def __init__(self, conn: sqlite3.Connection) -> None:
        self._conn = conn
        self._conn.row_factory = sqlite3.Row
        self._conn.execute("PRAGMA foreign_keys = ON")
        # Capture/live/worker/redrive open the DB concurrently; wait for the lock
        # instead of failing immediately with "database is locked".
        self._conn.execute("PRAGMA busy_timeout = 30000")
        self._in_transaction = False

    def _commit(self) -> None:
        """Commit — unless inside `transaction()`, which owns the commit."""
        if not self._in_transaction:
            self._conn.commit()

    @contextmanager
    def transaction(self) -> Iterator[None]:
        """Group several store calls into one atomic commit.

        Multi-step mutations (refine's hide-then-insert, redrive) run inside this
        so a crash between the steps leaves the database as if none of them
        happened. Individual store methods still commit themselves when called
        outside a transaction. Not reentrant — keep transactions short and flat.
        """
        if self._in_transaction:
            msg = "store transactions do not nest"
            raise RuntimeError(msg)
        self._in_transaction = True
        try:
            yield
            self._conn.commit()
        except BaseException:
            self._conn.rollback()
            raise
        finally:
            self._in_transaction = False

    @classmethod
    def connect(cls, path: Path) -> Self:
        """Connect without running migrations (fast path; schema assumed ready)."""
        path.parent.mkdir(parents=True, exist_ok=True)
        return cls(sqlite3.connect(path))

    @classmethod
    def open(cls, path: Path) -> Self:
        """Connect and bring the schema up to date. The canonical entry point."""
        store = cls.connect(path)
        # WAL: the six agents open this file concurrently; with the default
        # rollback journal every writer commit blocks all readers. Persistent per
        # database file, so setting it on each open is an idempotent no-op after
        # the first. (Skipped implicitly for :memory: stores — no WAL there.)
        store._conn.execute("PRAGMA journal_mode = WAL")
        store.migrate()
        return store

    @classmethod
    def memory(cls) -> Self:
        store = cls(sqlite3.connect(":memory:"))
        store.migrate()
        return store

    def migrate(self) -> None:
        """Apply any pending schema migrations (idempotent).

        Each step's DDL and its version bump commit together in one transaction
        (both are transactional in SQLite), so a step that fails partway — a crash,
        or a multi-statement step erroring after its first ALTER — rolls back wholly.
        The schema never ends up ahead of user_version, which would make the next
        run re-apply an already-applied ALTER and die with 'duplicate column'.
        """
        for attempt in range(2):
            version = int(self._conn.execute("PRAGMA user_version").fetchone()[0])
            try:
                for index in range(version, len(_MIGRATIONS)):
                    self._apply_migration(index)
            except sqlite3.OperationalError:
                # Another agent migrated between our version read and this apply
                # ("table … already exists"). Roll back the failed step and re-read
                # — the second pass applies whatever (if anything) is still
                # pending. A repeat failure is a real schema problem: raise it.
                if attempt:
                    raise
                self._conn.rollback()
                continue
            return

    def _apply_migration(self, index: int) -> None:
        # user_version takes no parameters; index+1 is our own integer.
        self._conn.executescript(
            f"BEGIN;\n{_MIGRATIONS[index]}\nPRAGMA user_version = {index + 1};\nCOMMIT;"
        )

    def schema_version(self) -> int:
        return int(self._conn.execute("PRAGMA user_version").fetchone()[0])

    def close(self) -> None:
        self._conn.close()

    # -- ingest ----------------------------------------------------------------

    def add_source(self, source: AudioSource) -> None:
        self._conn.execute(
            "INSERT OR IGNORE INTO sources (id, name, kind) VALUES (?, ?, ?)",
            (source.id, source.name, source.kind.value),
        )
        self._commit()

    def source_kind(self, source_id: str) -> SourceKind | None:
        """The registered kind of a source, or None if unknown — used to guard
        upload-only operations (rename/delete) off the household capture archive."""
        row = self._conn.execute(
            "SELECT kind FROM sources WHERE id = ?", (source_id,)
        ).fetchone()
        return SourceKind(str(row["kind"])) if row else None

    def source(self, source_id: str) -> AudioSource | None:
        """The full source record (id, name, kind), or None if unknown — the sync push
        needs the name and kind to register the source on the fleet."""
        row = self._conn.execute(
            "SELECT id, name, kind FROM sources WHERE id = ?", (source_id,)
        ).fetchone()
        if row is None:
            return None
        return AudioSource(
            id=str(row["id"]),
            name=str(row["name"]),
            kind=SourceKind(str(row["kind"])),
            spec="",
        )

    def source_span(self, source_id: str) -> tuple[datetime, datetime] | None:
        """The [first-start, last-end) covered by a source's audio, or None if it has
        no segments — for queuing a whole-session refine."""
        row = self._conn.execute(
            "SELECT MIN(start_utc) AS s, MAX(end_utc) AS e FROM audio_segments "
            "WHERE source_id = ?",
            (source_id,),
        ).fetchone()
        if row is None or row["s"] is None:
            return None
        return datetime.fromisoformat(row["s"]), datetime.fromisoformat(row["e"])

    def rename_source(self, source_id: str, name: str) -> None:
        """Rename a source (the sessions list's display title)."""
        self._conn.execute(
            "UPDATE sources SET name = ? WHERE id = ?", (name, source_id)
        )
        self._commit()

    def delete_source(self, source_id: str) -> list[str]:
        """Delete one source and everything derived from it — audio segments, their
        transcript turns (+ lineage/embeddings/corrections), and any queued
        refine/AB-compare work. Returns the audio file paths so the caller can unlink
        them. Atomic. The caller must confirm the source is an UPLOAD first: the
        continuous household capture is append-only and must never be deletable."""
        seg_rows = self._conn.execute(
            "SELECT id, path FROM audio_segments WHERE source_id = ?", (source_id,)
        ).fetchall()
        audio_ids = [int(r["id"]) for r in seg_rows]
        paths = [str(r["path"]) for r in seg_rows]
        with self.transaction():
            for audio_id in audio_ids:
                turn_ids = [
                    int(r["id"])
                    for r in self._conn.execute(
                        "SELECT id FROM transcript_segments WHERE audio_segment_id = ?",
                        (audio_id,),
                    ).fetchall()
                ]
                for turn_id in turn_ids:
                    self._conn.execute(
                        "DELETE FROM transcript_embeddings WHERE segment_id = ?",
                        (turn_id,),
                    )
                    self._conn.execute(
                        "DELETE FROM transcript_lineage WHERE derived_id = ? "
                        "OR source_id = ?",
                        (turn_id, turn_id),
                    )
                self._conn.execute(
                    "DELETE FROM corrections WHERE audio_segment_id = ?", (audio_id,)
                )
                self._conn.execute(
                    "DELETE FROM transcript_segments WHERE audio_segment_id = ?",
                    (audio_id,),
                )
            self._conn.execute(
                "DELETE FROM refine_requests WHERE source_id = ?", (source_id,)
            )
            self._conn.execute(
                "DELETE FROM ab_compare_runs WHERE source_id = ?", (source_id,)
            )
            self._conn.execute(
                "DELETE FROM audio_segments WHERE source_id = ?", (source_id,)
            )
            self._conn.execute("DELETE FROM sources WHERE id = ?", (source_id,))
        return paths

    # --- quiet-cleanup: cached raw volume + hard-delete of confirmed quiet spans ---

    def set_audio_measurement(
        self, audio_id: AudioSegmentId, mean_db: float | None, envelope: bytes
    ) -> None:
        """Cache what one decode of a capture segment yielded — its raw mean volume
        (dBFS) and its envelope — so no file is ever decoded for the cleanup twice.

        `mean_db` is None for a file that would not decode: it has been *examined* (an
        empty envelope is set), but it has no volume, and a segment without one is never
        quiet. Recording the verdict is the point — otherwise a corrupt file would be
        retried by every scan, for ever.
        """
        self._conn.execute(
            "UPDATE audio_segments SET mean_volume = ?, envelope = ? WHERE id = ?",
            (mean_db, envelope, int(audio_id)),
        )
        self._commit()

    def audio_segments_unmeasured(
        self, *, limit: int = 2000, kinds: Collection[SourceKind]
    ) -> list[tuple[AudioSegmentId, str]]:
        """(id, path) of segments from sources of `kinds` not yet measured. Keyed on the
        envelope, not the volume: a segment measured before envelopes were stored still
        owes us its shape, and the decode that gives us one gives us both."""
        placeholders = ",".join("?" * len(kinds))
        rows = self._conn.execute(
            "SELECT a.id, a.path FROM audio_segments a "
            "JOIN sources s ON s.id = a.source_id "
            f"WHERE a.envelope IS NULL AND s.kind IN ({placeholders}) "
            "ORDER BY a.start_utc LIMIT ?",
            (*[k.value for k in kinds], limit),
        ).fetchall()
        return [(AudioSegmentId(int(r["id"])), str(r["path"])) for r in rows]

    def measured_counts(self, *, kinds: Collection[SourceKind]) -> tuple[int, int]:
        """(measured, total) sweepable segments — the cleanup scan's progress."""
        placeholders = ",".join("?" * len(kinds))
        row = self._conn.execute(
            "SELECT count(a.envelope) AS done, count(*) AS total FROM audio_segments a "
            "JOIN sources s ON s.id = a.source_id "
            f"WHERE s.kind IN ({placeholders})",
            tuple(k.value for k in kinds),
        ).fetchone()
        return int(row["done"]), int(row["total"])

    def sweepable_source_ids(self) -> list[str]:
        """The continuously-recording sources — the ones a cleanup can act on, and so
        the ones worth measuring a sound threshold for."""
        rows = self._conn.execute(
            "SELECT id FROM sources WHERE kind != ? ORDER BY id",
            (SourceKind.UPLOAD.value,),
        ).fetchall()
        return [str(r["id"]) for r in rows]

    def quiet_envelopes(
        self, source_id: str, *, quiet_below_db: float, limit: int = 400
    ) -> list[bytes]:
        """Envelopes of one source's *idle* segments — quiet by volume, and with no
        turn standing on them. This is the mic breathing and nothing else: the sample
        its noise floor is measured from (recall.calibrate). The caller supplies the
        volume that counts as idle: that is detection's policy, not the store's."""
        rows = self._conn.execute(
            """SELECT a.envelope FROM audio_segments a
               WHERE a.source_id = ? AND a.mean_volume <= ? AND a.envelope IS NOT NULL
                 AND NOT EXISTS (SELECT 1 FROM transcript_segments t
                                 WHERE t.audio_segment_id = a.id
                                   AND t.superseded_by IS NULL
                                   AND t.hidden_reason IS NULL)
               LIMIT ?""",
            (source_id, quiet_below_db, limit),
        ).fetchall()
        return [bytes(r["envelope"]) for r in rows]

    def speech_envelopes(self, source_id: str, *, limit: int = 400) -> list[bytes]:
        """Envelopes of one source's segments that produced a turn that still stands —
        audio known to contain words. Their quietest peak is the floor under which this
        mic's threshold may never sit, or it would list none of them as sound."""
        rows = self._conn.execute(
            """SELECT DISTINCT a.envelope FROM audio_segments a
               JOIN transcript_segments t ON t.audio_segment_id = a.id
               WHERE a.source_id = ? AND a.envelope IS NOT NULL
                 AND t.superseded_by IS NULL AND t.hidden_reason IS NULL
               LIMIT ?""",
            (source_id, limit),
        ).fetchall()
        return [bytes(r["envelope"]) for r in rows]

    def audio_segments_to_analyse(
        self, *, kinds: Collection[SourceKind], limit: int = 200
    ) -> list[tuple[AudioSegmentId, str, str]]:
        """(id, path, source) of the segments a cleanup could act on but has not been
        listened to: measured, read by ASR, showing no turn — and not yet analysed.

        A segment showing a turn is already vetoed by the transcript and needs no VAD
        time; everything else is a candidate, *whatever its volume*. This once selected
        on `mean_volume <= -60` instead, to save the detector a pass over "obviously
        loud" audio. It saved nothing and cost the truth: a minute above that line was
        never listened to, so it stayed unknown for ever — and an unknown minute breaks
        a run. Twelve hours of the archive sat in that band, silently cutting hour-long
        silences into shards. The cheap filter was buying a wrong answer.
        """
        placeholders = ",".join("?" * len(kinds))
        rows = self._conn.execute(
            "SELECT a.id, a.path, a.source_id FROM audio_segments a "
            "JOIN sources s ON s.id = a.source_id "
            f"WHERE a.speech_s IS NULL AND s.kind IN ({placeholders}) "
            "AND a.mean_volume IS NOT NULL "
            "AND a.transcribed_utc IS NOT NULL "
            "AND NOT EXISTS (SELECT 1 FROM transcript_segments t "
            "                WHERE t.audio_segment_id = a.id "
            "                  AND t.superseded_by IS NULL "
            "                  AND t.hidden_reason IS NULL) "
            "ORDER BY a.start_utc LIMIT ?",
            (*[k.value for k in kinds], limit),
        ).fetchall()
        return [
            (AudioSegmentId(int(r["id"])), str(r["path"]), str(r["source_id"]))
            for r in rows
        ]

    def set_audio_analysis(
        self, audio_id: AudioSegmentId, speech_s: float, structure: float | None
    ) -> None:
        """Record what a segment holds: seconds of detected speech, and how far it
        departs from its mic's idle noise. `speech_s` is the cleanup's veto — 0.0 means
        the detector heard nothing, and only then may the audio be swept."""
        self._conn.execute(
            "UPDATE audio_segments SET speech_s = ?, structure = ? WHERE id = ?",
            (speech_s, structure, int(audio_id)),
        )
        self._commit()

    def analysed_counts(self, *, kinds: Collection[SourceKind]) -> tuple[int, int]:
        """(analysed, total) of the segments a cleanup could act on — how far the speech
        detector has got. The same population `audio_segments_to_analyse` draws from, so
        the progress bar counts what is actually queued."""
        placeholders = ",".join("?" * len(kinds))
        row = self._conn.execute(
            "SELECT count(a.speech_s) AS done, count(*) AS total FROM audio_segments a "
            "JOIN sources s ON s.id = a.source_id "
            f"WHERE s.kind IN ({placeholders}) "
            "AND a.mean_volume IS NOT NULL "
            "AND a.transcribed_utc IS NOT NULL "
            "AND NOT EXISTS (SELECT 1 FROM transcript_segments t "
            "                WHERE t.audio_segment_id = a.id "
            "                  AND t.superseded_by IS NULL "
            "                  AND t.hidden_reason IS NULL)",
            (*[k.value for k in kinds],),
        ).fetchone()
        return int(row["done"]), int(row["total"])

    def idle_segment_paths(
        self, source_id: str, *, quiet_below_db: float, limit: int = 24
    ) -> list[str]:
        """Paths of one source's idle segments — quiet, with no turn standing on them.
        The sample its noise fingerprint is learned from: this mic, hearing nothing."""
        rows = self._conn.execute(
            """SELECT a.path FROM audio_segments a
               WHERE a.source_id = ? AND a.mean_volume <= ?
                 AND NOT EXISTS (SELECT 1 FROM transcript_segments t
                                 WHERE t.audio_segment_id = a.id
                                   AND t.superseded_by IS NULL
                                   AND t.hidden_reason IS NULL)
               ORDER BY a.start_utc LIMIT ?""",
            (source_id, quiet_below_db, limit),
        ).fetchall()
        return [str(r["path"]) for r in rows]

    def span_structure(self, audio_ids: Sequence[AudioSegmentId]) -> float | None:
        """The most unusual moment across a span: the highest `structure` any of its
        segments reached. Max, not mean — a span with one cough in it is a span with a
        cough in it, and averaging that over an hour of nothing would hide it."""
        if not audio_ids:
            return None
        placeholders = ",".join("?" * len(audio_ids))
        row = self._conn.execute(
            "SELECT max(structure) AS peak FROM audio_segments "
            f"WHERE id IN ({placeholders})",
            tuple(int(a) for a in audio_ids),
        ).fetchone()
        if row is None or row["peak"] is None:
            return None
        return float(row["peak"])

    def segments_showing_no_turns(
        self,
    ) -> dict[AudioSegmentId, list[tuple[TranscriptId, str, str]]]:
        """Segments that once had turns and now show none — the ones a refine emptied.

        Returns each one's hidden turns as (id, hidden_reason, text) in id order, so the
        caller can pick the newest generation to bring back (recall.repair). Superseded
        turns are excluded: those were properly replaced, and their replacement stands.
        """
        rows = self._conn.execute(
            """SELECT t.audio_segment_id AS audio_id, t.id, t.hidden_reason, t.text
               FROM transcript_segments t
               WHERE t.audio_segment_id IS NOT NULL
                 AND t.hidden_reason IS NOT NULL
                 AND t.superseded_by IS NULL
                 AND NOT EXISTS (
                     SELECT 1 FROM transcript_segments v
                     WHERE v.audio_segment_id = t.audio_segment_id
                       AND v.superseded_by IS NULL AND v.hidden_reason IS NULL)
               ORDER BY t.audio_segment_id, t.id"""
        ).fetchall()
        blanked: dict[AudioSegmentId, list[tuple[TranscriptId, str, str]]] = {}
        for row in rows:
            audio_id = AudioSegmentId(int(row["audio_id"]))
            blanked.setdefault(audio_id, []).append(
                (
                    TranscriptId(int(row["id"])),
                    str(row["hidden_reason"]),
                    str(row["text"]),
                )
            )
        return blanked

    def segments_with_no_detected_speech(self) -> set[AudioSegmentId]:
        """Segments the speech detector listened to and heard nothing in."""
        rows = self._conn.execute(
            "SELECT id FROM audio_segments WHERE speech_s = 0.0"
        ).fetchall()
        return {AudioSegmentId(int(r["id"])) for r in rows}

    def machine_turns_on_silent_audio(self) -> list[tuple[TranscriptId, str]]:
        """Visible machine turns standing on audio the detector heard nothing in.

        Whisper hallucinates on silence, and such a turn is not evidence of speech — it
        is
        evidence of an empty room. Human turns are excluded: a person's judgement
        outranks
        a model's.
        """
        rows = self._conn.execute(
            """SELECT t.id, t.text FROM transcript_segments t
               JOIN audio_segments a ON a.id = t.audio_segment_id
               WHERE a.speech_s = 0.0
                 AND t.hidden_reason IS NULL AND t.superseded_by IS NULL
                 AND t.asr_model != ?
               ORDER BY t.id""",
            (HUMAN_MODEL,),
        ).fetchall()
        return [(TranscriptId(int(r["id"])), str(r["text"])) for r in rows]

    def set_source_noise_shape(self, source_id: str, shape: bytes) -> None:
        """Store a microphone's idle-noise fingerprint (see recall.spectrum)."""
        self._conn.execute(
            "UPDATE sources SET noise_shape = ? WHERE id = ?", (shape, source_id)
        )
        self._commit()

    def source_noise_shape(self, source_id: str) -> bytes | None:
        row = self._conn.execute(
            "SELECT noise_shape FROM sources WHERE id = ?", (source_id,)
        ).fetchone()
        if row is None or row["noise_shape"] is None:
            return None
        return bytes(row["noise_shape"])

    def set_source_event_db(self, source_id: str, event_db: float) -> None:
        """Record a microphone's measured sound threshold (dBFS)."""
        self._conn.execute(
            "UPDATE sources SET event_db = ? WHERE id = ?", (event_db, source_id)
        )
        self._commit()

    def source_event_db(self, source_id: str) -> float | None:
        """A microphone's measured sound threshold, None if it has not been measured."""
        row = self._conn.execute(
            "SELECT event_db FROM sources WHERE id = ?", (source_id,)
        ).fetchone()
        if row is None or row["event_db"] is None:
            return None
        return float(row["event_db"])

    def audio_envelopes(
        self, audio_ids: Sequence[AudioSegmentId]
    ) -> dict[AudioSegmentId, bytes]:
        """The stored envelopes of `audio_ids` — the review reads these instead of
        decoding. Segments measured before envelopes were kept are simply absent, and
        the caller decodes those (see recall.envelope.segment_envelope)."""
        if not audio_ids:
            return {}
        placeholders = ",".join("?" * len(audio_ids))
        rows = self._conn.execute(
            f"SELECT id, envelope FROM audio_segments WHERE id IN ({placeholders}) "
            "AND envelope IS NOT NULL",
            tuple(int(a) for a in audio_ids),
        ).fetchall()
        return {AudioSegmentId(int(r["id"])): bytes(r["envelope"]) for r in rows}

    def audio_segment_volumes(
        self, *, kinds: Collection[SourceKind]
    ) -> list[SegmentVolume]:
        """Every segment of a source in `kinds`, in time order — the input to quiet-span
        detection.

        Carries three things beyond the volume, because deleting a capture segment also
        deletes everything derived from it: the source (several mics record the same
        room at once, so runs must be grouped per source, never across them), whether
        ASR has examined the segment yet, and whether it left any *current, visible*
        turn behind — human or machine. A turn that stands is speech we chose to keep,
        and the audio under it is not idle noise however quiet its 60-second mean looks.

        `loud_fraction` is left None here and filled in by `recall.quiet`: measuring it
        needs the mic's calibrated threshold and a decode of the stored envelope, and
        the capture agent must be able to import this module without the ML stack.
        """
        placeholders = ",".join("?" * len(kinds))
        rows = self._conn.execute(
            f"""SELECT a.id, a.source_id, a.start_utc, a.end_utc, a.mean_volume,
                      a.transcribed_utc, a.speech_s, a.structure,
                      EXISTS (SELECT 1 FROM transcript_segments t
                              WHERE t.audio_segment_id = a.id
                                AND t.superseded_by IS NULL
                                AND t.hidden_reason IS NULL) AS has_speech
               FROM audio_segments a JOIN sources s ON s.id = a.source_id
               WHERE s.kind IN ({placeholders})
               ORDER BY a.start_utc""",
            tuple(k.value for k in kinds),
        ).fetchall()
        return [
            SegmentVolume(
                audio_id=AudioSegmentId(int(r["id"])),
                source_id=str(r["source_id"]),
                start=datetime.fromisoformat(r["start_utc"]),
                end=datetime.fromisoformat(r["end_utc"]),
                mean_db=None if r["mean_volume"] is None else float(r["mean_volume"]),
                transcribed=r["transcribed_utc"] is not None,
                has_speech=bool(r["has_speech"]),
                speech_s=None if r["speech_s"] is None else float(r["speech_s"]),
                structure=None if r["structure"] is None else float(r["structure"]),
            )
            for r in rows
        ]

    def audio_segments_between(
        self, source_id: str, start: datetime, end: datetime
    ) -> list[tuple[AudioSegmentId, str, datetime, datetime, float | None]]:
        """(id, path, start, end, mean_volume) of one source's capture segments
        overlapping [start, end), in time order — the input to the envelope the cleanup
        review draws. One source, because a waveform mixing two mics would show sound
        the span under review does not contain. Overlapping, not contained, so the
        segments at a span's edges (the ones that ended the quiet) are included."""
        _require_aware(start, "start")
        _require_aware(end, "end")
        rows = self._conn.execute(
            "SELECT id, path, start_utc, end_utc, mean_volume FROM audio_segments "
            "WHERE source_id = ? AND start_utc < ? AND end_utc > ? ORDER BY start_utc",
            (source_id, end.isoformat(), start.isoformat()),
        ).fetchall()
        return [
            (
                AudioSegmentId(int(r["id"])),
                str(r["path"]),
                datetime.fromisoformat(r["start_utc"]),
                datetime.fromisoformat(r["end_utc"]),
                None if r["mean_volume"] is None else float(r["mean_volume"]),
            )
            for r in rows
        ]

    def audio_segment_bounds(
        self, audio_ids: Sequence[AudioSegmentId]
    ) -> tuple[str, datetime, datetime] | None:
        """(source, first start, last end) of these segments — what a delete is about to
        destroy, in the terms a person would recognise it by. None if none of them exist
        (a duplicate request for a span that has already gone)."""
        if not audio_ids:
            return None
        placeholders = ",".join("?" * len(audio_ids))
        row = self._conn.execute(
            f"""SELECT source_id, MIN(start_utc) AS first, MAX(end_utc) AS last
                  FROM audio_segments WHERE id IN ({placeholders})""",
            tuple(int(a) for a in audio_ids),
        ).fetchone()
        if row is None or row["first"] is None:
            return None
        return (
            str(row["source_id"]),
            datetime.fromisoformat(row["first"]),
            datetime.fromisoformat(row["last"]),
        )

    def delete_audio_segments(self, audio_ids: Sequence[AudioSegmentId]) -> list[str]:
        """Hard-delete specific capture segments and all derived from them (turns and
        their lineage/embeddings/corrections/FTS), returning the audio file paths to
        unlink. For the quiet-cleanup: a human-confirmed span of total-quiet capture is
        truly removed to reclaim disk. Atomic."""
        paths: list[str] = []
        with self.transaction():
            for audio_id in audio_ids:
                row = self._conn.execute(
                    "SELECT path FROM audio_segments WHERE id = ?", (int(audio_id),)
                ).fetchone()
                if row is None:
                    continue
                paths.append(str(row["path"]))
                turn_ids = [
                    int(r["id"])
                    for r in self._conn.execute(
                        "SELECT id FROM transcript_segments WHERE audio_segment_id = ?",
                        (int(audio_id),),
                    ).fetchall()
                ]
                for turn_id in turn_ids:
                    self._conn.execute(
                        "DELETE FROM transcript_embeddings WHERE segment_id = ?",
                        (turn_id,),
                    )
                    self._conn.execute(
                        "DELETE FROM transcript_lineage WHERE derived_id = ? "
                        "OR source_id = ?",
                        (turn_id, turn_id),
                    )
                # transcript_fts is a contentless FTS5 table (no per-row DELETE); like
                # delete_source, leave its entries — a search rowid with no segment row
                # simply resolves to nothing.
                self._conn.execute(
                    "DELETE FROM corrections WHERE audio_segment_id = ?",
                    (int(audio_id),),
                )
                self._conn.execute(
                    "DELETE FROM transcript_segments WHERE audio_segment_id = ?",
                    (int(audio_id),),
                )
                self._conn.execute(
                    "DELETE FROM audio_segments WHERE id = ?", (int(audio_id),)
                )
        return paths

    def register_source(self, source: AudioSource) -> None:
        """Authoritative registration by the recording agent, which knows the source's
        true kind (the USB mic is coreaudio; a phone announces itself as tcp_pcm via
        the ingest handshake). Corrects a kind the worker may have guessed when it
        first discovered the directory, but preserves the name."""
        self._conn.execute(
            """INSERT INTO sources (id, name, kind, port) VALUES (?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   kind = excluded.kind, port = excluded.port""",
            (source.id, source.name, source.kind.value, source.port),
        )
        self._commit()

    def session_summaries(self) -> list[SessionSummary]:
        """Per uploaded session (a discrete recording, e.g. a meeting): id, name,
        span (first segment start → last segment end), visible-turn count, and a CSV
        of the people heard — for the sessions list. Newest first.

        Only *human-confirmed* speakers are named (a real name in speaker_label);
        everything else is 'unknown'. Voiceprint *guesses* are deliberately not shown:
        on out-of-domain audio (a visitor's voice) they match enrolled household
        members at high confidence — a guest can score 0.95 against a member — so no
        score floor separates true from false, and a bare name chip would assert a
        false attribution. Raw 'SPEAKER_nn' cluster tags are never
        surfaced as people either. Names appear here as they're confirmed in review."""
        rows = self._conn.execute(
            """SELECT s.id, s.name,
                      MIN(a.start_utc), MAX(a.end_utc),
                      COUNT(t.id),
                      GROUP_CONCAT(DISTINCT CASE
                          WHEN t.id IS NULL THEN NULL
                          WHEN t.speaker_label IS NOT NULL
                               AND t.speaker_label NOT LIKE 'SPEAKER_%'
                               THEN t.speaker_label
                          ELSE 'unknown'
                      END)
               FROM sources s
               JOIN audio_segments a ON a.source_id = s.id
               LEFT JOIN transcript_segments t
                      ON t.audio_segment_id = a.id
                     AND t.superseded_by IS NULL AND t.hidden_reason IS NULL
               WHERE s.kind = ?
               GROUP BY s.id
               ORDER BY MIN(a.start_utc) DESC""",
            (SourceKind.UPLOAD.value,),
        ).fetchall()
        return [
            SessionSummary(str(r[0]), str(r[1]), str(r[2]), str(r[3]), int(r[4]), r[5])
            for r in rows
        ]

    def add_audio_segment(self, segment: Segment) -> AudioSegmentId:
        self._conn.execute(
            """INSERT OR IGNORE INTO audio_segments
               (source_id, path, start_utc, end_utc, sample_rate, channels)
               VALUES (?, ?, ?, ?, ?, ?)""",
            (
                segment.source_id,
                segment.path,
                segment.start.isoformat(),
                segment.end.isoformat(),
                segment.sample_rate,
                segment.channels,
            ),
        )
        self._commit()
        row = self._conn.execute(
            "SELECT id FROM audio_segments WHERE source_id = ? AND start_utc = ?",
            (segment.source_id, segment.start.isoformat()),
        ).fetchone()
        return AudioSegmentId(int(row["id"]))

    def add_transcript_segment(  # noqa: PLR0913 - one kwarg per stored column
        self,
        *,
        audio_segment_id: int | None,
        start: datetime,
        end: datetime,
        text: str,
        asr_model: str,
        language: str | None = None,
        language_confidence: float | None = None,
        asr_confidence: float | None = None,
        speaker_label: str | None = None,
        speaker_id: int | None = None,
        speaker_cluster: str | None = None,
        provenance: str | None = None,
        created: datetime | None = None,
        word_timings: Sequence[Word] | None = None,
    ) -> TranscriptId:
        _require_aware(start, "start")
        _require_aware(end, "end")
        if created is not None:
            _require_aware(created, "created")
        cursor = self._conn.execute(
            """INSERT INTO transcript_segments
               (audio_segment_id, start_utc, end_utc, text, language,
                language_confidence, asr_confidence, asr_model, speaker_label,
                speaker_id, speaker_cluster, provenance, created_utc, word_timings)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                audio_segment_id,
                start.isoformat(),
                end.isoformat(),
                text,
                language,
                language_confidence,
                asr_confidence,
                asr_model,
                speaker_label,
                speaker_id,
                speaker_cluster,
                provenance,
                None if created is None else created.isoformat(),
                _dump_word_timings(word_timings),
            ),
        )
        segment_id = TranscriptId(int(cursor.lastrowid or 0))
        self._conn.execute(
            "INSERT INTO transcript_fts (rowid, text) VALUES (?, ?)",
            (segment_id, text),
        )
        self._commit()
        return segment_id

    def supersede(self, old_id: int, new_id: int) -> None:
        self._conn.execute(
            "UPDATE transcript_segments SET superseded_by = ? WHERE id = ?",
            (new_id, old_id),
        )
        self._commit()

    def supersede_many(
        self, source_ids: list[TranscriptId], new_id: TranscriptId
    ) -> None:
        """Replace several turns with one derived turn (e.g. merged fragments).

        Marks each source superseded by `new_id` and records the lineage so the
        many-to-one derivation is auditable and reversible.
        """
        for source_id in source_ids:
            self._conn.execute(
                "UPDATE transcript_segments SET superseded_by = ? WHERE id = ?",
                (new_id, source_id),
            )
            self._conn.execute(
                """INSERT OR IGNORE INTO transcript_lineage (derived_id, source_id)
                   VALUES (?, ?)""",
                (new_id, source_id),
            )
        self._commit()

    def hide(self, transcript_id: int, reason: str) -> None:
        """Soft-hide a turn (e.g. a confirmed hallucination) with a reason.

        It leaves all current views but is never deleted — fully recoverable.
        """
        self._conn.execute(
            "UPDATE transcript_segments SET hidden_reason = ? WHERE id = ?",
            (reason, transcript_id),
        )
        self._commit()

    def claim_hidden(self, transcript_id: int, reason: str) -> bool:
        """Hide a turn only if it's still current — a single atomic statement, so when
        the same turn is split concurrently (an impatient double-tap) exactly one
        caller wins. Returns True if this call hid it, False if it was already
        hidden/superseded (a concurrent split got there first; this caller must not
        also split it).
        """
        cursor = self._conn.execute(
            "UPDATE transcript_segments SET hidden_reason = ? "
            "WHERE id = ? AND hidden_reason IS NULL AND superseded_by IS NULL",
            (reason, transcript_id),
        )
        self._commit()
        return cursor.rowcount == 1

    def unhide(self, transcript_id: int) -> None:
        """Restore a soft-hidden turn (recover a false-positive hide)."""
        self._conn.execute(
            "UPDATE transcript_segments SET hidden_reason = NULL WHERE id = ?",
            (transcript_id,),
        )
        self._commit()

    def unhide_all(self, reason: str) -> int:
        """Restore every turn hidden with `reason` (reset a scan). Returns count."""
        cursor = self._conn.execute(
            "UPDATE transcript_segments SET hidden_reason = NULL "
            "WHERE hidden_reason = ?",
            (reason,),
        )
        self._commit()
        return cursor.rowcount

    def frequent_machine_texts(self, *, min_count: int) -> set[str]:
        """Machine-turn texts that recur at least `min_count` times.

        The data-derived "filler vocabulary" — the repeated phrases Whisper emits
        on silence ("Gracias.", "So", "Thank you."). Novel, one-off utterances are
        never in this set, so they're protected from the hallucination scan.
        """
        rows = self._conn.execute(
            """SELECT text FROM transcript_segments
               WHERE asr_model != ? GROUP BY text HAVING count(*) >= ?""",
            (HUMAN_MODEL, min_count),
        ).fetchall()
        return {str(r["text"]) for r in rows}

    def visible_machine_turns_for_audio(
        self, audio_segment_id: int
    ) -> list[TranscriptSegment]:
        """Current, visible, machine-authored turns for one audio segment.

        The unit the hallucination scan inspects: human turns and already-hidden
        ones are left alone.
        """
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE audio_segment_id = ? AND superseded_by IS NULL
                 AND hidden_reason IS NULL AND asr_model != ?
               ORDER BY start_utc""",
            (audio_segment_id, HUMAN_MODEL),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def audio_segment_ids_with_machine_turns(self) -> list[AudioSegmentId]:
        """Distinct audio segments that have current visible machine turns."""
        rows = self._conn.execute(
            """SELECT DISTINCT audio_segment_id FROM transcript_segments
               WHERE audio_segment_id IS NOT NULL AND superseded_by IS NULL
                 AND hidden_reason IS NULL AND asr_model != ?
               ORDER BY audio_segment_id""",
            (HUMAN_MODEL,),
        ).fetchall()
        return [AudioSegmentId(int(r["audio_segment_id"])) for r in rows]

    def visible_machine_turns(self) -> list[TranscriptSegment]:
        """All current, visible, machine-authored turns (incl. live, no audio)."""
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
                 AND asr_model != ?
               ORDER BY start_utc""",
            (HUMAN_MODEL,),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def _segments_without_marker(
        self, marker: str, *, limit: int, newest_first: bool = False
    ) -> list[AudioSegmentId]:
        """Audio segments that still have visible machine turns but no turn yet
        hidden with `marker` (a 'reason' prefix). The hidden-turn marker is what
        makes a re-derive pass resumable + chunkable."""
        # `order` is a controlled literal (not user input), so inlining it is safe.
        order = "DESC" if newest_first else "ASC"
        rows = self._conn.execute(
            "SELECT DISTINCT audio_segment_id FROM transcript_segments "
            "WHERE audio_segment_id IS NOT NULL AND superseded_by IS NULL "
            "AND hidden_reason IS NULL AND asr_model != ? "
            "AND audio_segment_id NOT IN ("
            "  SELECT audio_segment_id FROM transcript_segments "
            "  WHERE hidden_reason LIKE ? AND audio_segment_id IS NOT NULL) "
            f"ORDER BY audio_segment_id {order} LIMIT ?",
            (HUMAN_MODEL, marker + "%", limit),
        ).fetchall()
        return [AudioSegmentId(int(r["audio_segment_id"])) for r in rows]

    def audio_segments_to_redrive(self, *, limit: int) -> list[AudioSegmentId]:
        """Segments still needing the basic re-derive (no 'reprocessed' marker)."""
        return self._segments_without_marker(REPROCESSED_MARKER, limit=limit)

    def audio_segments_to_diarize(self, *, limit: int) -> list[AudioSegmentId]:
        """Segments still needing diarized refinement (no 'diarized' marker).
        Newest-first, so the most recent (most relevant) audio is refined first."""
        return self._segments_without_marker(
            DIARIZED_MARKER, limit=limit, newest_first=True
        )

    def audio_segments_to_rediarize(self, *, limit: int) -> list[AudioSegmentId]:
        """Segments diarized by an *older* pipeline: visible diarized turns whose
        provenance predates ALIGNED_MARKER. Re-diarizing upgrades them to the current
        pipeline; it re-tags them ALIGNED_MARKER, so the pass terminates. Newest-first.
        """
        rows = self._conn.execute(
            "SELECT DISTINCT audio_segment_id FROM transcript_segments "
            "WHERE audio_segment_id IS NOT NULL AND superseded_by IS NULL "
            "AND hidden_reason IS NULL AND asr_model != ? "
            "AND provenance LIKE ? AND provenance NOT LIKE ? "
            "ORDER BY audio_segment_id DESC LIMIT ?",
            (HUMAN_MODEL, DIARIZED_MARKER + "%", ALIGNED_MARKER + "%", limit),
        ).fetchall()
        return [AudioSegmentId(int(r["audio_segment_id"])) for r in rows]

    def audio_segments_for_source(
        self, source: str, *, limit: int
    ) -> list[AudioSegmentId]:
        """Every audio segment of one source, oldest-first — for a forced full
        re-derive of a single recording through the canonical diarized pipeline,
        regardless of each segment's current diarization state."""
        rows = self._conn.execute(
            "SELECT id FROM audio_segments WHERE source_id = ? "
            "ORDER BY start_utc LIMIT ?",
            (source, limit),
        ).fetchall()
        return [AudioSegmentId(int(r["id"])) for r in rows]

    def audio_segments_in_range(
        self, source: str, start: datetime, end: datetime, *, limit: int
    ) -> list[AudioSegmentId]:
        """Audio segments of `source` whose span overlaps [start, end), oldest-first —
        for refining just a chosen stretch of a recording, not the whole thing."""
        _require_aware(start, "start")
        _require_aware(end, "end")
        rows = self._conn.execute(
            "SELECT id FROM audio_segments WHERE source_id = ? "
            "AND start_utc < ? AND end_utc > ? ORDER BY start_utc LIMIT ?",
            (source, end.isoformat(), start.isoformat(), limit),
        ).fetchall()
        return [AudioSegmentId(int(r["id"])) for r in rows]

    def audio_segment_intervals(
        self, source: str, *, since: datetime
    ) -> list[tuple[datetime, datetime]]:
        """(start, end) of `source`'s audio segments ending at or after `since`,
        oldest-first — the recorded coverage a loss check reconciles against the
        pause/resume events to tell a deliberate pause from silently lost speech."""
        _require_aware(since, "since")
        rows = self._conn.execute(
            "SELECT start_utc, end_utc FROM audio_segments "
            "WHERE source_id = ? AND end_utc >= ? ORDER BY start_utc",
            (source, since.isoformat()),
        ).fetchall()
        return [
            (
                datetime.fromisoformat(r["start_utc"]),
                datetime.fromisoformat(r["end_utc"]),
            )
            for r in rows
        ]

    def add_capture_event(
        self,
        kind: CaptureEventKind,
        *,
        utc: datetime,
        source_id: str | None = None,
        detail: str | None = None,
    ) -> int:
        """Append an immutable capture-lifecycle event (pause / resume / dead-window).

        This is the durable record that tells a deliberate pause-gap apart from silently
        lost audio: the timeline gap alone can't. `utc` is when the event happened (the
        pause instant, or the dead segment's own timestamp), not when it was noticed.
        Append-only — an event is never edited. Returns the event id.
        """
        _require_aware(utc, "utc")
        cursor = self._conn.execute(
            "INSERT INTO capture_events (utc, kind, source_id, detail) "
            "VALUES (?, ?, ?, ?)",
            (utc.isoformat(), kind, source_id, detail),
        )
        self._commit()
        return int(cursor.lastrowid or 0)

    def capture_events_since(
        self, since: datetime, *, kinds: tuple[CaptureEventKind, ...] | None = None
    ) -> list[CaptureEvent]:
        """Capture events at or after `since`, oldest-first; optionally only `kinds`."""
        _require_aware(since, "since")
        sql = (
            "SELECT id, utc, kind, source_id, detail FROM capture_events WHERE utc >= ?"
        )
        params: list[str] = [since.isoformat()]
        if kinds is not None:
            sql += f" AND kind IN ({','.join('?' * len(kinds))})"
            params.extend(kinds)
        sql += " ORDER BY utc, id"
        rows = self._conn.execute(sql, params).fetchall()
        return [
            CaptureEvent(
                id=int(r["id"]),
                utc=datetime.fromisoformat(r["utc"]),
                kind=str(r["kind"]),
                source_id=r["source_id"],
                detail=r["detail"],
            )
            for r in rows
        ]

    def add_refine_request(self, source: str, start: datetime, end: datetime) -> int:
        """Queue an on-demand refine of [start, end) of `source`; the idle daemon runs
        it. Returns the request id."""
        _require_aware(start, "start")
        _require_aware(end, "end")
        cursor = self._conn.execute(
            "INSERT INTO refine_requests (source_id, start_utc, end_utc, created_utc) "
            "VALUES (?, ?, ?, ?)",
            (source, start.isoformat(), end.isoformat(), datetime.now(UTC).isoformat()),
        )
        self._commit()
        return int(cursor.lastrowid or 0)

    def pending_refine_requests(self, *, limit: int = 100) -> list[RefineRequest]:
        """Queued refine requests not yet processed, oldest-first."""
        rows = self._conn.execute(
            "SELECT id, source_id, start_utc, end_utc FROM refine_requests "
            "WHERE done_utc IS NULL ORDER BY id LIMIT ?",
            (limit,),
        ).fetchall()
        return [
            RefineRequest(
                id=int(r["id"]),
                source=str(r["source_id"]),
                start=datetime.fromisoformat(r["start_utc"]),
                end=datetime.fromisoformat(r["end_utc"]),
            )
            for r in rows
        ]

    def mark_refine_request_done(self, request_id: int) -> None:
        """Mark a refine request processed, so the daemon won't run it again."""
        self._conn.execute(
            "UPDATE refine_requests SET done_utc = ? WHERE id = ?",
            (datetime.now(UTC).isoformat(), request_id),
        )
        self._commit()

    def pending_upload_jobs(self, *, limit: int = 100) -> list[UploadJob]:
        """Uploaded-session segments not yet through ASR, oldest-first — the fleet-side
        upload queue served to the Mac (which holds the ML). Derived from the segment
        rows themselves, so nothing has to remember to enqueue; done = mark_transcribed.
        """
        rows = self._conn.execute(
            "SELECT a.id, a.source_id, s.name, a.path, a.start_utc, a.end_utc, "
            "a.sample_rate, a.channels "
            "FROM audio_segments a JOIN sources s ON s.id = a.source_id "
            "WHERE s.kind = ? AND a.transcribed_utc IS NULL "
            "ORDER BY a.start_utc LIMIT ?",
            (SourceKind.UPLOAD.value, limit),
        ).fetchall()
        return [
            UploadJob(
                audio_id=int(r["id"]),
                source=str(r["source_id"]),
                title=str(r["name"]),
                file=Path(str(r["path"])).name,
                start=datetime.fromisoformat(r["start_utc"]),
                end=datetime.fromisoformat(r["end_utc"]),
                sample_rate=int(r["sample_rate"]),
                channels=int(r["channels"]),
            )
            for r in rows
        ]

    def add_ab_compare_run(  # noqa: PLR0913 - source + window + the two models
        self,
        source: str,
        start: datetime | None,
        end: datetime | None,
        *,
        model_a: str,
        model_b: str,
        base_model: str,
        fleet_id: int | None = None,
    ) -> int:
        """Queue an A/B comparison of `model_a` vs `model_b` over `source` (the whole
        recording, or [start, end) if given). The runner drains it. Returns the id.
        `fleet_id` marks a run mirrored from the fleet's queue (the Isis split), so
        its result can be pushed back to that row."""
        if start is not None:
            _require_aware(start, "start")
        if end is not None:
            _require_aware(end, "end")
        cursor = self._conn.execute(
            "INSERT INTO ab_compare_runs "
            "(source_id, start_utc, end_utc, model_a, model_b, base_model, status, "
            "created_utc, fleet_id) VALUES (?, ?, ?, ?, ?, ?, 'queued', ?, ?)",
            (
                source,
                start.isoformat() if start else None,
                end.isoformat() if end else None,
                model_a,
                model_b,
                base_model,
                datetime.now(UTC).isoformat(),
                fleet_id,
            ),
        )
        self._commit()
        return int(cursor.lastrowid or 0)

    def pending_ab_compare_runs(self, *, limit: int = 100) -> list[AbCompareJob]:
        """Queued runs not yet started, oldest-first (without result_json)."""
        rows = self._conn.execute(
            f"SELECT {_AB_COMPARE_COLS} FROM ab_compare_runs "
            "WHERE status = 'queued' ORDER BY id LIMIT ?",
            (limit,),
        ).fetchall()
        return [_row_to_ab_compare_job(r, with_result=False) for r in rows]

    def unfinished_ab_compare_runs(self, *, limit: int = 100) -> list[AbCompareJob]:
        """Runs still awaiting a result (queued or running), oldest-first — what the
        fleet serves to the Mac across the split. Running rows stay served so a Mac
        that lost its local mirror re-adopts them instead of wedging them forever."""
        rows = self._conn.execute(
            f"SELECT {_AB_COMPARE_COLS} FROM ab_compare_runs "
            "WHERE status IN ('queued', 'running') ORDER BY id LIMIT ?",
            (limit,),
        ).fetchall()
        return [_row_to_ab_compare_job(r, with_result=False) for r in rows]

    def ab_compare_run_by_fleet_id(self, fleet_id: int) -> AbCompareJob | None:
        """The local mirror of a fleet-queued run (with its result_json, for the
        push-back), or None if this Mac has not adopted it yet."""
        row = self._conn.execute(
            f"SELECT {_AB_COMPARE_COLS}, result_json FROM ab_compare_runs "
            "WHERE fleet_id = ?",
            (fleet_id,),
        ).fetchone()
        return _row_to_ab_compare_job(row, with_result=True) if row else None

    def list_ab_compare_runs(self, *, limit: int = 100) -> list[AbCompareJob]:
        """All comparison runs, newest-first, without the (large) result_json."""
        rows = self._conn.execute(
            f"SELECT {_AB_COMPARE_COLS} FROM ab_compare_runs ORDER BY id DESC LIMIT ?",
            (limit,),
        ).fetchall()
        return [_row_to_ab_compare_job(r, with_result=False) for r in rows]

    def get_ab_compare_run(self, run_id: int) -> AbCompareJob | None:
        """One comparison run including its result_json, or None if unknown."""
        row = self._conn.execute(
            f"SELECT {_AB_COMPARE_COLS}, result_json FROM ab_compare_runs WHERE id = ?",
            (run_id,),
        ).fetchone()
        return _row_to_ab_compare_job(row, with_result=True) if row else None

    def mark_ab_compare_running(self, run_id: int) -> None:
        """Mark a run started, so a second runner won't pick it up."""
        self._conn.execute(
            "UPDATE ab_compare_runs SET status = 'running', started_utc = ? "
            "WHERE id = ?",
            (datetime.now(UTC).isoformat(), run_id),
        )
        self._commit()

    def save_ab_compare_result(  # noqa: PLR0913 - the report's denormalized summary
        self,
        run_id: int,
        *,
        result_json: str,
        mean_wer_a: float | None,
        mean_wer_b: float | None,
        n_corrections: int,
        n_segments: int,
        n_changed: int,
    ) -> None:
        """Store a finished run's report + denormalized summary; mark it done."""
        self._conn.execute(
            "UPDATE ab_compare_runs SET status = 'done', done_utc = ?, "
            "result_json = ?, mean_wer_a = ?, mean_wer_b = ?, n_corrections = ?, "
            "n_segments = ?, n_changed = ? WHERE id = ?",
            (
                datetime.now(UTC).isoformat(),
                result_json,
                mean_wer_a,
                mean_wer_b,
                n_corrections,
                n_segments,
                n_changed,
                run_id,
            ),
        )
        self._commit()

    def mark_ab_compare_error(self, run_id: int, message: str) -> None:
        """Mark a run failed with `message`, so it isn't retried; the UI shows it."""
        self._conn.execute(
            "UPDATE ab_compare_runs SET status = 'error', done_utc = ?, error = ? "
            "WHERE id = ?",
            (datetime.now(UTC).isoformat(), message, run_id),
        )
        self._commit()

    def media_spans(
        self, *, max_gap_s: float, min_duration_s: float
    ) -> list[tuple[datetime, datetime]]:
        """Long, dense runs of back-to-back turns — likely TV/film, not the family.

        A run is consecutive current turns with gaps <= max_gap_s; runs lasting
        >= min_duration_s are returned. Used to deprioritise media in the labeling
        queue (a 2-hour movie is one span; real conversation is burstier/shorter).
        """
        rows = self._conn.execute(
            """SELECT start_utc, end_utc FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
               ORDER BY start_utc"""
        ).fetchall()
        spans: list[tuple[datetime, datetime]] = []
        run_start: datetime | None = None
        run_end: datetime | None = None
        for r in rows:
            s = datetime.fromisoformat(r["start_utc"])
            e = datetime.fromisoformat(r["end_utc"])
            if run_start is None or run_end is None:
                run_start, run_end = s, e
            elif (s - run_end).total_seconds() <= max_gap_s:
                run_end = max(run_end, e)
            else:
                if (run_end - run_start).total_seconds() >= min_duration_s:
                    spans.append((run_start, run_end))
                run_start, run_end = s, e
        if (
            run_start is not None
            and run_end is not None
            and (run_end - run_start).total_seconds() >= min_duration_s
        ):
            spans.append((run_start, run_end))
        return spans

    def record_split(self, old_id: int, new_ids: list[int]) -> None:
        """Replace one turn with several derived ones (split into speakers).

        The original is superseded (it drops from current views) and every new
        fragment's lineage to it is recorded — one-to-many provenance.
        """
        if not new_ids:
            return
        self._conn.execute(
            "UPDATE transcript_segments SET superseded_by = ? WHERE id = ?",
            (new_ids[0], old_id),
        )
        for new_id in new_ids:
            self._conn.execute(
                "INSERT OR IGNORE INTO transcript_lineage (derived_id, source_id) "
                "VALUES (?, ?)",
                (new_id, old_id),
            )
        self._commit()

    def sources_of(self, derived_id: int) -> list[int]:
        """The transcript ids a derived turn was built from (lineage)."""
        rows = self._conn.execute(
            "SELECT source_id FROM transcript_lineage WHERE derived_id = ? "
            "ORDER BY source_id",
            (derived_id,),
        ).fetchall()
        return [int(r["source_id"]) for r in rows]

    def current_version(self, transcript_id: int) -> TranscriptSegment | None:
        """Follow the supersede chain from `transcript_id` to the live version.

        A deep link to a turn that has since been corrected/reprocessed resolves
        to the current text, not the stale original.
        """
        seg = self.get_transcript(transcript_id)
        seen = {transcript_id}
        while seg is not None and seg.superseded_by is not None:
            nxt = seg.superseded_by
            if nxt in seen:  # guard against a cycle
                break
            seen.add(nxt)
            seg = self.get_transcript(nxt)
        return seg

    def human_corrections_overlapping(
        self, audio_segment_id: int, start: datetime, end: datetime
    ) -> list[Correction]:
        """Human corrections whose audio span overlaps [start, end) in this file.

        Lets a re-segmentation pass defer to human ground truth by audio time, so
        corrections survive even when turn boundaries change underneath them.
        """
        _require_aware(start, "start")
        _require_aware(end, "end")
        rows = self._conn.execute(
            """SELECT id, audio_segment_id, start_utc, end_utc, corrected_text,
                      language
               FROM corrections
               WHERE audio_segment_id = ? AND start_utc < ? AND end_utc > ?
               ORDER BY start_utc""",
            (audio_segment_id, end.isoformat(), start.isoformat()),
        ).fetchall()
        return [
            Correction(
                id=CorrectionId(int(r["id"])),
                audio_segment_id=_opt_audio_id(r["audio_segment_id"]),
                start=datetime.fromisoformat(r["start_utc"]),
                end=datetime.fromisoformat(r["end_utc"]),
                corrected_text=str(r["corrected_text"]),
                language=_opt_str(r["language"]),
            )
            for r in rows
        ]

    # -- query -----------------------------------------------------------------

    def search(self, query: str, *, limit: int = 50) -> list[TranscriptSegment]:
        # LEFT JOIN the audio segment's source so a hit carries which recorder caught
        # it (LEFT, so an audio-less turn still matches). ts has no source_id column,
        # so a.source_id doesn't collide.
        rows = self._conn.execute(
            """SELECT ts.*, a.source_id FROM transcript_segments ts
               JOIN transcript_fts ON transcript_fts.rowid = ts.id
               LEFT JOIN audio_segments a ON a.id = ts.audio_segment_id
               WHERE transcript_fts MATCH ? AND ts.superseded_by IS NULL
                 AND ts.hidden_reason IS NULL
               ORDER BY ts.start_utc
               LIMIT ?""",
            (query, limit),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def turns_by_id(self, ids: Sequence[int]) -> list[TranscriptSegment]:
        """Specific turns by id, with their capturing source — for inspection. Returned
        in the order requested; ids with no row are skipped. Unlike `search`, a turn is
        returned even if superseded or hidden — you asked for that exact id."""
        if not ids:
            return []
        # The placeholders are a literal "?,?,…" (one per id); the ids bind as params,
        # so this is not string-interpolated user input.
        placeholders = ",".join("?" * len(ids))
        rows = self._conn.execute(
            f"""SELECT ts.*, a.source_id FROM transcript_segments ts
                LEFT JOIN audio_segments a ON a.id = ts.audio_segment_id
                WHERE ts.id IN ({placeholders})""",
            tuple(ids),
        ).fetchall()
        by_id = {int(row["id"]): _row_to_segment(row) for row in rows}
        return [by_id[i] for i in ids if i in by_id]

    def moment_coverage(self, start: datetime, end: datetime) -> list[SourceCoverage]:
        """Per source, for the window [start, end): whether its raw audio overlaps it
        (who recorded the moment) and how many current turns it has there (who actually
        transcribed it). A source can record without transcribing — phone audio too
        faint to clear VAD/ASR shows recorded=True, turns=0. Sorted by source id."""
        _require_aware(start, "start")
        _require_aware(end, "end")
        recorded = {
            str(row["source_id"])
            for row in self._conn.execute(
                "SELECT DISTINCT source_id FROM audio_segments "
                "WHERE start_utc < ? AND end_utc > ?",
                (end.isoformat(), start.isoformat()),
            ).fetchall()
        }
        counts = {
            str(row["src"]): int(row["n"])
            for row in self._conn.execute(
                """SELECT a.source_id src, count(*) n
                   FROM transcript_segments t
                   JOIN audio_segments a ON a.id = t.audio_segment_id
                   WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL
                     AND t.start_utc < ? AND t.end_utc > ?
                   GROUP BY a.source_id""",
                (end.isoformat(), start.isoformat()),
            ).fetchall()
        }
        return [
            SourceCoverage(source_id=s, recorded=s in recorded, turns=counts.get(s, 0))
            for s in sorted(recorded | set(counts))
        ]

    def segments_in_range(
        self, start: datetime, end: datetime
    ) -> list[TranscriptSegment]:
        _require_aware(start, "start")
        _require_aware(end, "end")
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
                 AND start_utc >= ? AND start_utc < ?
               ORDER BY start_utc""",
            (start.isoformat(), end.isoformat()),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def recent_transcripts(
        self,
        *,
        limit: int = 200,
        before: datetime | None = None,
        after: datetime | None = None,
        source: str | None = None,
    ) -> list[TranscriptSegment]:
        """Current transcripts, for paging the timeline either direction.

        `before`: the newest `limit` turns older than the cursor (newest-first) —
        page back. `after`: the oldest `limit` turns newer than the cursor
        (oldest-first) — page forward, contiguous with what's already loaded.
        `source`: restrict to one recorder/session (e.g. a meeting upload).

        The cursor on the wire is a bare start time, and turns can share one
        (co-located mics, corrections), so a full page extends past `limit` to
        include every turn tied with its boundary — a page that split the group
        would make the next strict-< page silently skip the group's remainder.
        Callers therefore treat "page length >= limit" as has-more.
        """
        # Join the audio segment's source so the timeline can fold same-moment
        # turns across mics; LEFT JOIN keeps source-less turns (e.g. corrections
        # with no audio segment). start_utc is on both tables, so qualify it.
        select = (
            "SELECT t.*, a.source_id FROM transcript_segments t "
            "LEFT JOIN audio_segments a ON t.audio_segment_id = a.id "
            "WHERE t.superseded_by IS NULL AND t.hidden_reason IS NULL"
        )
        where = [""]
        params: list[str | int] = []
        if source is not None:
            where.append("AND a.source_id = ?")
            params.append(source)
        if before is not None:
            _require_aware(before, "before")
            where.append("AND t.start_utc < ?")
            params.append(before.isoformat())
        if after is not None:
            _require_aware(after, "after")
            where.append("AND t.start_utc > ?")
            params.append(after.isoformat())
        # Forward paging takes the page adjacent to the cursor (oldest-first); every
        # other case is newest-first. The id tiebreak makes same-instant order
        # deterministic (and matches the tie-extension below).
        order = "ASC" if after is not None else "DESC"
        rows = self._conn.execute(
            f"{select}{' '.join(where)} ORDER BY t.start_utc {order}, t.id {order}"
            " LIMIT ?",
            [*params, limit],
        ).fetchall()
        if rows and len(rows) == limit:
            # Full page: pull in the boundary's remaining ties (see docstring). The
            # ties satisfy the before/after bounds by having the boundary's own time.
            boundary = rows[-1]["start_utc"]
            seen = [row["id"] for row in rows if row["start_utc"] == boundary]
            marks = ",".join("?" * len(seen))
            rows += self._conn.execute(
                f"{select}{' '.join(where)} AND t.start_utc = ?"
                f" AND t.id NOT IN ({marks}) ORDER BY t.id {order}",
                [*params, boundary, *seen],
            ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def pending_audio_segments(self) -> list[Segment]:
        """Captured audio not yet through ASR (the worker queue).

        Keyed on `transcribed_utc IS NULL`, not on the absence of transcript rows:
        a VAD-gated pass that finds no speech is still *processed* and must not be
        retried, even though it wrote zero turns.
        """
        rows = self._conn.execute(
            """SELECT * FROM audio_segments
               WHERE transcribed_utc IS NULL
               ORDER BY start_utc"""
        ).fetchall()
        return [
            Segment(
                source_id=str(row["source_id"]),
                sequence=0,
                start=datetime.fromisoformat(row["start_utc"]),
                end=datetime.fromisoformat(row["end_utc"]),
                path=str(row["path"]),
                sample_rate=int(row["sample_rate"]),
                channels=int(row["channels"]),
            )
            for row in rows
        ]

    def audio_segment(self, audio_segment_id: AudioSegmentId) -> Segment | None:
        """Fetch one captured audio segment by id (for re-deriving its turns)."""
        row = self._conn.execute(
            "SELECT * FROM audio_segments WHERE id = ?", (audio_segment_id,)
        ).fetchone()
        if row is None:
            return None
        return Segment(
            source_id=str(row["source_id"]),
            sequence=0,
            start=datetime.fromisoformat(row["start_utc"]),
            end=datetime.fromisoformat(row["end_utc"]),
            path=str(row["path"]),
            sample_rate=int(row["sample_rate"]),
            channels=int(row["channels"]),
        )

    def mark_transcribed(self, audio_segment_id: int) -> None:
        """Record that a segment has been through ASR (speech found or not)."""
        self._conn.execute(
            "UPDATE audio_segments SET transcribed_utc = end_utc WHERE id = ?",
            (audio_segment_id,),
        )
        self._commit()

    def add_vocabulary_term(self, term: str) -> int:
        """Add a term to the household vocabulary (idempotent by exact term)."""
        cleaned = term.strip()
        if not cleaned:
            msg = "vocabulary term must not be blank"
            raise ValueError(msg)
        self._conn.execute(
            """INSERT INTO vocabulary (term, created_utc) VALUES (?, ?)
               ON CONFLICT(term) DO NOTHING""",
            (cleaned, datetime.now(UTC).isoformat()),
        )
        self._commit()
        row = self._conn.execute(
            "SELECT id FROM vocabulary WHERE term = ?", (cleaned,)
        ).fetchone()
        return int(row["id"])

    def vocabulary_terms(self) -> list[VocabularyTerm]:
        rows = self._conn.execute(
            "SELECT id, term FROM vocabulary ORDER BY term COLLATE NOCASE"
        ).fetchall()
        return [VocabularyTerm(id=int(r["id"]), term=str(r["term"])) for r in rows]

    def delete_vocabulary_term(self, term_id: int) -> None:
        self._conn.execute("DELETE FROM vocabulary WHERE id = ?", (term_id,))
        self._commit()

    def set_day_summary(self, day: str, text: str, *, model: str) -> None:
        """Store (or re-derive) one day's summary — a derived view, so overwrite."""
        self._conn.execute(
            """INSERT INTO day_summaries (day, text, model, created_utc)
               VALUES (?, ?, ?, ?)
               ON CONFLICT(day) DO UPDATE
               SET text = excluded.text, model = excluded.model,
                   created_utc = excluded.created_utc""",
            (day, text, model, datetime.now(UTC).isoformat()),
        )
        self._commit()

    def get_day_summary(self, day: str) -> str | None:
        row = self._conn.execute(
            "SELECT text FROM day_summaries WHERE day = ?", (day,)
        ).fetchone()
        return None if row is None else str(row["text"])

    def recent_day_summaries(self, *, limit: int = 7) -> list[tuple[str, str, str]]:
        """Newest-first (day, text, model) — the Ask page's recent-days digest."""
        rows = self._conn.execute(
            "SELECT day, text, model FROM day_summaries ORDER BY day DESC LIMIT ?",
            (limit,),
        ).fetchall()
        return [(str(r["day"]), str(r["text"]), str(r["model"])) for r in rows]

    def get_setting(self, key: str) -> str | None:
        """A free-form setting, or None when unset/blank (callers `if value:`)."""
        row = self._conn.execute(
            "SELECT value FROM settings WHERE key = ?", (key,)
        ).fetchone()
        value = None if row is None else str(row["value"]).strip()
        return value or None

    def set_setting(self, key: str, value: str) -> None:
        self._conn.execute(
            """INSERT INTO settings (key, value) VALUES (?, ?)
               ON CONFLICT(key) DO UPDATE SET value = excluded.value""",
            (key, value),
        )
        self._commit()

    def set_live_summary(
        self, day: str, text: str, *, model: str, watermark: str
    ) -> None:
        """Cache the running day's provisional summary. One-row cache: writing a
        new day evicts any older one (the day rolled over; the settled summary in
        day_summaries takes it from there)."""
        self._conn.execute("DELETE FROM live_summaries WHERE day != ?", (day,))
        self._conn.execute(
            """INSERT INTO live_summaries
               (day, text, model, watermark, generated_utc)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(day) DO UPDATE
               SET text = excluded.text, model = excluded.model,
                   watermark = excluded.watermark,
                   generated_utc = excluded.generated_utc""",
            (day, text, model, watermark, datetime.now(UTC).isoformat()),
        )
        self._commit()

    def get_live_summary(self, day: str) -> LiveSummary | None:
        row = self._conn.execute(
            "SELECT * FROM live_summaries WHERE day = ?", (day,)
        ).fetchone()
        if row is None:
            return None
        return LiveSummary(
            day=str(row["day"]),
            text=str(row["text"]),
            model=str(row["model"]),
            watermark=str(row["watermark"]),
            generated_utc=str(row["generated_utc"]),
        )

    def day_watermark(self, day: str) -> str | None:
        """A fingerprint of the day's visible-turn state — the live summary's
        freshness key, or None when the day has no visible turns. Hashes the
        (id, speaker label) pairs, so it moves on anything that could change a
        regenerated summary: new turns, hides/supersedes, and speaker-label
        edits — labels change rows in place, so the max-id watermark this
        replaces never moved and human annotation couldn't reach the summary."""
        rows = self._conn.execute(
            """SELECT id, COALESCE(speaker_label, '') AS label
               FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
                 AND substr(start_utc, 1, 10) = ?
               ORDER BY id""",
            (day,),
        ).fetchall()
        if not rows:
            return None
        state = "\n".join(f"{r['id']}={r['label']}" for r in rows)
        return hashlib.sha256(state.encode()).hexdigest()[:16]

    def days_missing_summaries(self, *, limit: int = 30) -> list[str]:
        """Days (UTC, yyyy-mm-dd) that have visible turns but no summary yet —
        the work-list the idle summariser drains, oldest first."""
        rows = self._conn.execute(
            """SELECT DISTINCT substr(start_utc, 1, 10) AS day
               FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
                 AND day NOT IN (SELECT day FROM day_summaries)
               ORDER BY day LIMIT ?""",
            (limit,),
        ).fetchall()
        return [str(r["day"]) for r in rows]

    def short_audio_segments(
        self, *, max_seconds: float
    ) -> list[tuple[AudioSegmentId, Segment]]:
        """Segments whose stored duration is under `max_seconds` — the candidates
        for the reprobe repair (rows indexed while their file was still growing)."""
        rows = self._conn.execute(
            """SELECT * FROM audio_segments
               WHERE (julianday(end_utc) - julianday(start_utc)) * 86400 < ?
               ORDER BY start_utc""",
            (max_seconds,),
        ).fetchall()
        return [
            (
                AudioSegmentId(int(row["id"])),
                Segment(
                    source_id=str(row["source_id"]),
                    sequence=0,
                    start=datetime.fromisoformat(row["start_utc"]),
                    end=datetime.fromisoformat(row["end_utc"]),
                    path=str(row["path"]),
                    sample_rate=int(row["sample_rate"]),
                    channels=int(row["channels"]),
                ),
            )
            for row in rows
        ]

    def update_audio_segment_end(
        self, audio_segment_id: AudioSegmentId, end: datetime
    ) -> None:
        """Correct a segment's recorded end time (the reprobe repair)."""
        _require_aware(end, "end")
        self._conn.execute(
            "UPDATE audio_segments SET end_utc = ? WHERE id = ?",
            (end.isoformat(), audio_segment_id),
        )
        self._commit()

    def reprocessable_segments(
        self, *, max_confidence: float | None = None
    ) -> list[TranscriptSegment]:
        """Current, non-human segments eligible for re-transcription.

        Human-authored (corrected) segments are excluded — reprocessing improves
        machine output but never overwrites ground truth.
        """
        sql = [
            "SELECT * FROM transcript_segments",
            "WHERE superseded_by IS NULL AND hidden_reason IS NULL AND asr_model != ?",
            "AND audio_segment_id IS NOT NULL",
        ]
        params: list[str | float] = [HUMAN_MODEL]
        if max_confidence is not None:
            sql.append("AND (asr_confidence IS NULL OR asr_confidence < ?)")
            params.append(max_confidence)
        sql.append("ORDER BY start_utc")
        rows = self._conn.execute(" ".join(sql), params).fetchall()
        return [_row_to_segment(row) for row in rows]

    # A live turn is "caught up to" only where the archive actually processed the
    # audio of its moment — a transcribed audio segment spanning it. Not a blanket
    # "before the latest archive turn" watermark: capture can write empty files on
    # start (cleared as dead stubs, no audio segment) so a later segment exists while
    # an earlier stretch was never recorded. A watermark hides the live turns in that
    # gap even though nothing replaced them — and they are the ONLY record of that
    # moment. Coverage by segment span (not by where archive *turns* land) also means
    # a fully-transcribed silent minute correctly reconciles the noise the live pass
    # guessed inside it.
    _LIVE_COVERED = """
        EXISTS (
            SELECT 1 FROM audio_segments a
            WHERE a.transcribed_utc IS NOT NULL
              AND a.start_utc <= transcript_segments.start_utc
              AND a.end_utc   >  transcript_segments.start_utc
        )
    """

    def hide_provisional_covered(self) -> int:
        """Hide live transcripts a transcribed audio segment actually spans.

        Hidden, not superseded: there is no single archive turn that is "the
        better version" of a live turn, so a deep-linked live turn must keep
        resolving to itself. Returns how many were hidden.
        """
        cursor = self._conn.execute(
            f"""UPDATE transcript_segments SET hidden_reason = ?
               WHERE asr_model = ? AND superseded_by IS NULL
                 AND hidden_reason IS NULL AND {self._LIVE_COVERED}""",
            (RECONCILED_MARKER, LIVE_MODEL),
        )
        self._commit()
        return cursor.rowcount

    def restore_uncovered_provisional(self) -> int:
        """Un-hide live turns reconciled with no covering audio (loss repair).

        The old watermark reconcile hid every live turn before the latest archive
        transcript — including moments the archive never covered (empty-start
        segments cleared as dead stubs). Those live turns are the sole record of
        their moment. Restore any reconciled live turn no transcribed audio segment
        spans; the predicate is the exact inverse of `hide_provisional_covered`, so a
        turn a segment does span stays hidden and nothing ever double-shows.
        Idempotent.
        """
        cursor = self._conn.execute(
            f"""UPDATE transcript_segments SET hidden_reason = NULL
               WHERE asr_model = ? AND hidden_reason = ?
                 AND superseded_by IS NULL AND NOT {self._LIVE_COVERED}""",
            (LIVE_MODEL, RECONCILED_MARKER),
        )
        self._commit()
        return cursor.rowcount

    def visible_live_turns_since(
        self, watermark: int, *, limit: int = 500
    ) -> list[TranscriptSegment]:
        """Current (visible) live turns with id > `watermark`, oldest id first — the
        fast provisional transcripts the fleet's UI shows before the archive catches up.
        Reconciled (hidden) live turns are excluded: the clean segment carries their
        content to the fleet, so re-pushing them would only churn. Bounded, so one push
        pass is O(new)."""
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE asr_model = ? AND superseded_by IS NULL AND hidden_reason IS NULL
                 AND id > ?
               ORDER BY id LIMIT ?""",
            (LIVE_MODEL, watermark, limit),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def hide_live_turns_covered_by(self, seg_start: datetime, seg_end: datetime) -> int:
        """Hide visible live turns an incoming archive segment spans — the fleet-side
        mirror of `hide_provisional_covered`, run when a clean segment arrives so the
        fleet swaps the provisional live turn for the archive version instead of showing
        both. Same predicate: the live turn's start falls within the segment's span."""
        cursor = self._conn.execute(
            """UPDATE transcript_segments SET hidden_reason = ?
               WHERE asr_model = ? AND superseded_by IS NULL AND hidden_reason IS NULL
                 AND start_utc >= ? AND start_utc < ?""",
            (RECONCILED_MARKER, LIVE_MODEL, seg_start.isoformat(), seg_end.isoformat()),
        )
        self._commit()
        return cursor.rowcount

    def live_turn_present(self, start: datetime, text: str) -> bool:
        """Whether a live turn with this start+text already exists (in any state). The
        fleet ingest is idempotent: a retried push never duplicates a turn, nor
        resurrects one the archive already reconciled to hidden."""
        row = self._conn.execute(
            "SELECT 1 FROM transcript_segments "
            "WHERE asr_model = ? AND start_utc = ? AND text = ? LIMIT 1",
            (LIVE_MODEL, start.isoformat(), text),
        ).fetchone()
        return row is not None

    def get_transcript(self, segment_id: int) -> TranscriptSegment | None:
        row = self._conn.execute(
            "SELECT * FROM transcript_segments WHERE id = ?", (segment_id,)
        ).fetchone()
        return None if row is None else _row_to_segment(row)

    def training_queue(  # noqa: PLR0913 - filter band + window + ordering knobs
        self,
        *,
        min_confidence: float,
        max_confidence: float,
        limit: int = 40,
        since: datetime | None = None,
        until: datetime | None = None,
        order: str = "loudness",
    ) -> list[TranscriptSegment]:
        """Audible-but-uncertain machine turns for labeling, within a confidence
        band (above `min_confidence`, below `max_confidence`; NULL excluded) and an
        optional [`since`, `until`) time window.

        `order` picks how the cap selects candidates — "loudness" (loudest/clearest
        first; the labeling default, so a busy window's clear audio isn't crowded
        out by merely-confident quiet turns) or "time" (oldest first, to read a
        conversation in sequence). Unmeasured loudness sorts last, then confidence.
        """
        sql = [
            "SELECT * FROM transcript_segments",
            "WHERE superseded_by IS NULL AND hidden_reason IS NULL",
            "AND asr_model != ? AND audio_segment_id IS NOT NULL",
            "AND asr_confidence IS NOT NULL",
            "AND asr_confidence >= ? AND asr_confidence < ?",
            # No backchannels: sub-2s or few-word turns are poor ASR training
            # labels (padded to Whisper's 30s window they teach early-EOS), so
            # the labeling queue doesn't offer them. Word count approximated by
            # space count; still correctable from the timeline/session views.
            "AND (julianday(end_utc) - julianday(start_utc)) * 86400 >= 2.0",
            "AND length(text) - length(replace(text, ' ', '')) >= 3",
        ]
        params: list[str | float | int] = [HUMAN_MODEL, min_confidence, max_confidence]
        if since is not None:
            sql.append("AND start_utc >= ?")
            params.append(since.isoformat())
        if until is not None:
            sql.append("AND start_utc < ?")
            params.append(until.isoformat())
        if order == "time":
            sql.append("ORDER BY start_utc ASC LIMIT ?")
        else:
            sql.append(
                "ORDER BY (loudness IS NULL), loudness DESC, "
                "asr_confidence DESC, start_utc DESC LIMIT ?"
            )
        params.append(limit)
        rows = self._conn.execute(" ".join(sql), params).fetchall()
        return [_row_to_segment(row) for row in rows]

    def set_loudness(self, segment_id: int, value: float) -> None:
        """Persist a turn's measured loudness (speech_level) so the labeling queue
        can rank by it without re-decoding the audio on the request path.
        """
        self._conn.execute(
            "UPDATE transcript_segments SET loudness = ? WHERE id = ?",
            (value, segment_id),
        )
        self._commit()

    def segments_missing_loudness(self, *, limit: int = 200) -> list[TranscriptSegment]:
        """Current, visible machine turns whose loudness isn't measured yet — the
        offline backfill's work-list (each one is a sox decode, done off the
        request path). Newest first, so fresh capture becomes labelable soonest.
        """
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE loudness IS NULL AND superseded_by IS NULL
                 AND hidden_reason IS NULL AND asr_model != ?
                 AND audio_segment_id IS NOT NULL
               ORDER BY start_utc DESC LIMIT ?""",
            (HUMAN_MODEL, limit),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def set_word_timings(self, segment_id: int, words: Sequence[Word]) -> None:
        """Persist per-word timings on a turn (e.g. a human turn aligned to ASR), so it
        can be split/played audio-exactly like a diarized turn."""
        self._conn.execute(
            "UPDATE transcript_segments SET word_timings = ? WHERE id = ?",
            (_dump_word_timings(words), segment_id),
        )
        self._commit()

    def human_turns_missing_word_timings(
        self, *, limit: int = 50
    ) -> list[TranscriptSegment]:
        """Current human-corrected turns with audio but no word timings — the backfill's
        work-list. A correction is typed text, so it carries no timings; aligning it to
        word-level ASR makes splits/tight playback on it exact too. Newest first.
        """
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE asr_model = ? AND word_timings IS NULL
                 AND superseded_by IS NULL AND hidden_reason IS NULL
                 AND audio_segment_id IS NOT NULL
               ORDER BY start_utc DESC LIMIT ?""",
            (HUMAN_MODEL, limit),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def set_speaker_guess(self, segment_id: int, name: str, score: float) -> None:
        """Persist a turn's best-matching enrolled voice and its match strength.

        Display-only (the timeline shows "name score%"); never touches the human-
        confirmed speaker_label.
        """
        self._conn.execute(
            "UPDATE transcript_segments SET speaker_guess = ?, speaker_score = ? "
            "WHERE id = ?",
            (name, score, segment_id),
        )
        self._commit()

    def set_speaker_guesses(self, updates: Sequence[tuple[int, str, float]]) -> None:
        """Bulk-update guesses (id, name, score) in one transaction — the cheap
        re-match refreshes many turns at once when voiceprints change."""
        self._conn.executemany(
            "UPDATE transcript_segments SET speaker_guess = ?, speaker_score = ? "
            "WHERE id = ?",
            [(name, score, sid) for sid, name, score in updates],
        )
        self._commit()

    def name_voice(self, source_id: str, cluster: str, name: str | None) -> int:
        """Human-name a diarization voice across a source: set speaker_label on every
        current turn of that cluster (name=None clears it). Returns turns updated.

        A display label only — no correction or voiceprint is recorded, so naming a
        meeting's clinician doesn't enrol them as a household voice. This is the
        authoritative human naming of a voice; it overrides the auto guess.

        Deliberately no hidden_reason filter (unlike the read-side queries):
        hiding is a display state, but who spoke is a fact about the turn — a
        hidden turn that is later unhidden must come back correctly named.
        """
        cur = self._conn.execute(
            "UPDATE transcript_segments SET speaker_label = ? WHERE id IN ("
            "SELECT ts.id FROM transcript_segments ts "
            "JOIN audio_segments a ON a.id = ts.audio_segment_id "
            "WHERE a.source_id = ? AND ts.speaker_cluster = ? "
            "AND ts.superseded_by IS NULL)",
            (name, source_id, cluster),
        )
        self._commit()
        return cur.rowcount

    def session_voice_suggestions(
        self, source_id: str, *, min_score: float = 0.6
    ) -> dict[str, str]:
        """Suggest a name for each diarization voice in a session from its turns' cached
        voiceprint guesses. A cluster whose turns consistently match one enrolled voice
        gets that name; each name goes to its single best-matching cluster (so a
        clinician's weak false-matches can't claim a household name), and clusters below
        `min_score` get no suggestion — named by hand. Returns {cluster: name}.
        """
        rows = self._conn.execute(
            "SELECT ts.speaker_cluster cl, ts.speaker_guess g, ts.speaker_score s "
            "FROM transcript_segments ts "
            "JOIN audio_segments a ON a.id = ts.audio_segment_id "
            "WHERE a.source_id = ? AND ts.speaker_cluster IS NOT NULL "
            "AND ts.speaker_guess IS NOT NULL AND ts.superseded_by IS NULL "
            "AND ts.hidden_reason IS NULL",
            (source_id,),
        ).fetchall()

        # Per cluster: its dominant guess and the mean score of the turns that made it.
        by_cluster: dict[str, list[tuple[str, float]]] = {}
        for row in rows:
            by_cluster.setdefault(row["cl"], []).append((row["g"], row["s"] or 0.0))

        candidates: list[tuple[float, str, str]] = []  # (confidence, cluster, name)
        for cluster, guesses in by_cluster.items():
            counts: dict[str, int] = {}
            for guess, _ in guesses:
                counts[guess] = counts.get(guess, 0) + 1
            name = max(counts, key=lambda k: counts[k])
            scores = [s for g, s in guesses if g == name]
            conf = sum(scores) / len(scores) if scores else 0.0
            candidates.append((conf, cluster, name))

        # Greedy by confidence: each name and each cluster assigned at most once.
        candidates.sort(reverse=True)
        assigned: dict[str, str] = {}
        taken: set[str] = set()
        for conf, cluster, name in candidates:
            if conf >= min_score and name not in taken and cluster not in assigned:
                assigned[cluster] = name
                taken.add(name)
        return assigned

    def set_turn_speaker(self, segment_id: int, name: str | None) -> None:
        """Set/clear the human speaker label on one turn — reassign a mis-diarized turn
        to the right voice. Display label only (no correction/voiceprint)."""
        self._conn.execute(
            "UPDATE transcript_segments SET speaker_label = ? WHERE id = ?",
            (name, segment_id),
        )
        self._commit()

    def nudge_turn(self, segment_id: int, edge: str, delta: float) -> None:
        """Move one edge ('start'/'end') of a turn by `delta` seconds (signed), clamped
        to the audio segment and a 0.1s minimum span — hand-tune a split boundary by ear
        when the aligner's cut is slightly off. Playback follows the span, so the bubble
        then plays exactly the trimmed audio.
        """
        turn = self.get_transcript(segment_id)
        if turn is None or turn.audio_segment_id is None:
            return
        seg = self.audio_segment(turn.audio_segment_id)
        if seg is None:
            return
        min_span = timedelta(seconds=0.1)
        start, end = turn.start, turn.end
        shift = timedelta(seconds=delta)
        if edge == "start":
            start = max(seg.start, min(turn.start + shift, end - min_span))
        elif edge == "end":
            end = min(seg.end, max(turn.end + shift, start + min_span))
        else:
            return
        # Word timings are stored relative to the turn START, so a start trim
        # shifts every word by the trim; words that fall outside the new span are
        # dropped (their audio is no longer part of the turn) and boundary words
        # are clipped. Without this, every later audio-exact split and tight
        # playback would be off by exactly the trim.
        words = _rebase_word_timings(
            turn.word_timings,
            shift=(turn.start - start).total_seconds(),
            duration=(end - start).total_seconds(),
        )
        self._conn.execute(
            """UPDATE transcript_segments
               SET start_utc = ?, end_utc = ?, word_timings = ?
               WHERE id = ?""",
            (start.isoformat(), end.isoformat(), _dump_word_timings(words), segment_id),
        )
        self._commit()

    def known_speaker_names(self) -> list[str]:
        """Distinct speaker names already in use — enrolled household voices plus any
        human-assigned label — for autocompleting new names, so one person is spelled
        the same everywhere (a clinician named in one meeting suggests in the next)."""
        rows = self._conn.execute(
            "SELECT name FROM speakers "
            "UNION "
            "SELECT DISTINCT speaker_label FROM transcript_segments "
            "WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%' "
            "ORDER BY name COLLATE NOCASE"
        ).fetchall()
        return [r[0] for r in rows if r[0]]

    def session_turn_ids(self, source_id: str) -> list[int]:
        """The current (visible) turns of a session in time order — the sequence a span
        assignment walks across."""
        rows = self._conn.execute(
            """SELECT ts.id FROM transcript_segments ts
               JOIN audio_segments a ON a.id = ts.audio_segment_id
               WHERE a.source_id = ? AND ts.superseded_by IS NULL
                 AND ts.hidden_reason IS NULL
               ORDER BY ts.start_utc""",
            (source_id,),
        ).fetchall()
        return [int(r["id"]) for r in rows]

    def session_turns(self, source_id: str) -> list[TranscriptSegment]:
        """Every current turn of a session, oldest first — the whole call, for reading
        or handing to a reviewer."""
        rows = self._conn.execute(
            """SELECT t.*, a.source_id FROM transcript_segments t
               JOIN audio_segments a ON a.id = t.audio_segment_id
               WHERE a.source_id = ? AND t.superseded_by IS NULL
                 AND t.hidden_reason IS NULL
               ORDER BY t.start_utc""",
            (source_id,),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def set_embedding(self, segment_id: int, vector: Sequence[float]) -> None:
        """Persist a turn's voiceprint vector (embed once; re-match for free)."""
        self._conn.execute(
            "INSERT OR REPLACE INTO transcript_embeddings (segment_id, vector) "
            "VALUES (?, ?)",
            (segment_id, json.dumps([float(x) for x in vector])),
        )
        self._commit()

    def segments_missing_embedding(
        self, *, limit: int = 200
    ) -> list[TranscriptSegment]:
        """Current, visible machine turns not yet embedded — the *embed-once*
        work-list (each is a pyannote embedding, done off the request path).
        Human-labelled turns are authoritative and skipped; too-short clips can't
        be embedded. Newest first, so fresh capture gets a voiceprint soonest.
        """
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE id NOT IN (SELECT segment_id FROM transcript_embeddings)
                 AND speaker_label IS NULL AND superseded_by IS NULL
                 AND hidden_reason IS NULL AND asr_model != ?
                 AND audio_segment_id IS NOT NULL
                 AND (julianday(end_utc) - julianday(start_utc)) * 86400 >= ?
               ORDER BY start_utc DESC LIMIT ?""",
            (HUMAN_MODEL, _MIN_GUESS_DURATION_S, limit),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def embeddings_with_guesses(
        self,
    ) -> list[tuple[int, list[float], str | None, float | None]]:
        """Every current, guessable turn that has a stored embedding, with its
        current cached guess — the input to the cheap re-match. Returns
        (segment_id, vector, speaker_guess, speaker_score)."""
        rows = self._conn.execute(
            """SELECT te.segment_id AS id, te.vector AS vector,
                      ts.speaker_guess AS guess, ts.speaker_score AS score
               FROM transcript_embeddings te
               JOIN transcript_segments ts ON ts.id = te.segment_id
               WHERE ts.superseded_by IS NULL AND ts.hidden_reason IS NULL
                 AND ts.speaker_label IS NULL"""
        ).fetchall()
        return [
            (
                int(r["id"]),
                json.loads(r["vector"]),
                _opt_str(r["guess"]),
                _opt_float(r["score"]),
            )
            for r in rows
        ]

    def low_confidence_segments(
        self, *, max_confidence: float, limit: int = 50
    ) -> list[TranscriptSegment]:
        """Current segments most in need of human review (lowest confidence first).

        NULL confidence sorts first (unknown == most-suspect). This is the active-
        learning queue: the labels that improve the model most per minute spent.
        """
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
                 AND (asr_confidence IS NULL OR asr_confidence < ?)
               ORDER BY asr_confidence ASC, start_utc
               LIMIT ?""",
            (max_confidence, limit),
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def audio_segment_paths(self) -> list[tuple[int, str]]:
        """All (id, path) for audio segments — for maintenance like transcoding."""
        rows = self._conn.execute(
            "SELECT id, path FROM audio_segments ORDER BY id"
        ).fetchall()
        return [(int(row["id"]), str(row["path"])) for row in rows]

    def relink_audio_segment(self, audio_segment_id: int, new_path: str) -> None:
        """Re-point a segment at a new file (e.g. after transcoding FLAC->Opus)."""
        self._conn.execute(
            "UPDATE audio_segments SET path = ? WHERE id = ?",
            (new_path, audio_segment_id),
        )
        self._commit()

    def audio_segment_ref(
        self, audio_segment_id: AudioSegmentId
    ) -> tuple[str, datetime] | None:
        """The (path, start) of a raw audio segment, for slicing review clips."""
        row = self._conn.execute(
            "SELECT path, start_utc FROM audio_segments WHERE id = ?",
            (audio_segment_id,),
        ).fetchone()
        if row is None:
            return None
        return str(row["path"]), datetime.fromisoformat(row["start_utc"])

    def add_correction(  # noqa: PLR0913 - one kwarg per stored column
        self,
        *,
        transcript_segment_id: int,
        audio_segment_id: int | None,
        start: datetime,
        end: datetime,
        original_text: str,
        corrected_text: str,
        language: str | None,
        created: datetime,
        speaker: str | None = None,
        audio_confidence: float | None = None,
    ) -> int:
        cursor = self._conn.execute(
            """INSERT INTO corrections
               (transcript_segment_id, audio_segment_id, start_utc, end_utc,
                original_text, corrected_text, language, created_utc, speaker,
                audio_confidence)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
            (
                transcript_segment_id,
                audio_segment_id,
                start.isoformat(),
                end.isoformat(),
                original_text,
                corrected_text,
                language,
                created.isoformat(),
                speaker,
                audio_confidence,
            ),
        )
        self._commit()
        return int(cursor.lastrowid or 0)

    def correction_count(self) -> int:
        row = self._conn.execute(
            "SELECT count(*) AS n FROM corrections WHERE hidden_reason IS NULL"
        ).fetchone()
        return int(row["n"])

    def corrected_texts(self) -> set[str]:
        """Normalised texts already in the corpus — the labelling queue uses this
        to deprioritise re-labelling a phrase that's already been taught.
        """
        rows = self._conn.execute(
            "SELECT corrected_text FROM corrections WHERE hidden_reason IS NULL"
        ).fetchall()
        return {normalize_text(str(r["corrected_text"])) for r in rows}

    def corrections_by_speaker(self) -> dict[str, int]:
        """How many labelled fragments exist per speaker (untagged under "").
        Drives the labeling UI's balance display so no one voice is starved.
        """
        rows = self._conn.execute(
            "SELECT COALESCE(speaker, '') AS s, count(*) AS n FROM corrections "
            "WHERE hidden_reason IS NULL GROUP BY COALESCE(speaker, '')"
        ).fetchall()
        return {str(r["s"]): int(r["n"]) for r in rows}

    def _count(self, sql: str) -> int:
        return int(self._conn.execute(sql).fetchone()["n"])

    def source_rows(self) -> list[SourceRow]:
        """Registered sources — for the fleet liveness view. The kind parses through
        the enum: an unknown kind in the DB fails loud here, not as a silently
        never-matching string downstream."""
        rows = self._conn.execute(
            "SELECT id, name, kind FROM sources ORDER BY id"
        ).fetchall()
        return [
            SourceRow(str(r["id"]), str(r["name"]), SourceKind(str(r["kind"])))
            for r in rows
        ]

    def unidentified_segments(self) -> list[TranscriptSegment]:
        """Current segments with audio but no resolved speaker yet."""
        rows = self._conn.execute(
            """SELECT * FROM transcript_segments
               WHERE superseded_by IS NULL AND hidden_reason IS NULL
                 AND speaker_id IS NULL AND audio_segment_id IS NOT NULL
               ORDER BY start_utc"""
        ).fetchall()
        return [_row_to_segment(row) for row in rows]

    def resolve_speaker(
        self, transcript_segment_id: TranscriptId, speaker_id: SpeakerId
    ) -> None:
        self._conn.execute(
            "UPDATE transcript_segments SET speaker_id = ? WHERE id = ?",
            (speaker_id, transcript_segment_id),
        )
        self._commit()

    def enroll_speaker(
        self,
        name: str,
        embedding: Sequence[float],
        *,
        now: datetime,
        source_correction_id: int | None = None,
        source_segment_id: int | None = None,
    ) -> SpeakerId:
        """Add a reference voiceprint for `name` (creating the speaker if new).

        `source_segment_id` links the voiceprint to the labelled turn it was built from
        (the current source of truth, so a re-assignment can prune and re-derive it);
        `source_correction_id` is the legacy link to a tagged correction. The enrolment
        backfill keys on these so it never re-embeds the same clip.
        """
        self._conn.execute("INSERT OR IGNORE INTO speakers (name) VALUES (?)", (name,))
        speaker_id = self.speaker_id_for(name)
        if speaker_id is None:  # pragma: no cover - just inserted above
            msg = f"failed to enroll speaker {name!r}"
            raise RuntimeError(msg)
        self._conn.execute(
            """INSERT INTO speaker_embeddings
               (speaker_id, vector, created_utc,
                source_correction_id, source_segment_id)
               VALUES (?, ?, ?, ?, ?)""",
            (
                speaker_id,
                json.dumps(list(embedding)),
                now.isoformat(),
                source_correction_id,
                source_segment_id,
            ),
        )
        self._commit()
        return speaker_id

    def turns_needing_voiceprint(
        self,
        *,
        min_seconds: float = _MIN_VOICEPRINT_SECONDS,
        min_loudness: float = _MIN_VOICEPRINT_LOUDNESS,
        limit: int = 50,
    ) -> list[PendingVoiceprint]:
        """Current human-labelled turns not yet enrolled as a voiceprint — the
        enrolment backfill's work-list. The source of truth is `speaker_label` (a real
        name, set by a text correction *or* a session-view assign), so all human speaker
        work teaches the voices, not only text edits.

        Gated for clip quality: a turn shorter than `min_seconds` or quieter than
        `min_loudness` is skipped (a sliver/near-silent clip enrols a useless print).
        Unknown loudness (not yet measured) is kept. Gating touches enrolment only — the
        turn's text and display label are unaffected. Newest first.
        """
        rows = self._conn.execute(
            """SELECT id, speaker_label, audio_segment_id, start_utc, end_utc
               FROM transcript_segments
               WHERE speaker_label IS NOT NULL AND speaker_label NOT LIKE 'SPEAKER%'
                 AND superseded_by IS NULL AND hidden_reason IS NULL
                 AND audio_segment_id IS NOT NULL
                 AND (julianday(end_utc) - julianday(start_utc)) * 86400 >= ?
                 AND (loudness IS NULL OR loudness >= ?)
                 AND id NOT IN (
                   SELECT source_segment_id FROM speaker_embeddings
                   WHERE source_segment_id IS NOT NULL
                 )
               ORDER BY start_utc DESC LIMIT ?""",
            (min_seconds, min_loudness, limit),
        ).fetchall()
        return [
            PendingVoiceprint(
                segment_id=int(r["id"]),
                speaker=str(r["speaker_label"]),
                audio_segment_id=AudioSegmentId(int(r["audio_segment_id"])),
                start=datetime.fromisoformat(r["start_utc"]),
                end=datetime.fromisoformat(r["end_utc"]),
            )
            for r in rows
        ]

    def prune_stale_voiceprints(self) -> int:
        """Drop reference voiceprints that no longer reflect a current human label, so
        prints stay derived from the present turns. Returns how many were dropped.

        Two cases: (a) a turn-sourced print whose turn was superseded/hidden (a
        re-split) or re-assigned to a different name (its `speaker_label` no longer
        matches the enrolled speaker); and (b) a legacy correction-sourced print (no
        `source_segment_id`) *once that speaker already has a turn-sourced print* — so
        the transition is gap-free (no voice is ever left without any print mid-
        rebuild). The dropped turns re-enrol via `turns_needing_voiceprint`.
        """
        stale = self._conn.execute(
            """DELETE FROM speaker_embeddings
               WHERE source_segment_id IS NOT NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM transcript_segments t
                   JOIN speakers s ON s.id = speaker_embeddings.speaker_id
                   WHERE t.id = speaker_embeddings.source_segment_id
                     AND t.superseded_by IS NULL AND t.hidden_reason IS NULL
                     AND t.speaker_label = s.name
                 )"""
        ).rowcount
        # Speakers that now have a turn-sourced print — their legacy rows can retire.
        covered = [
            int(r[0])
            for r in self._conn.execute(
                "SELECT DISTINCT speaker_id FROM speaker_embeddings "
                "WHERE source_segment_id IS NOT NULL"
            ).fetchall()
        ]
        retired = 0
        if covered:
            placeholders = ",".join("?" * len(covered))
            retired = self._conn.execute(
                f"""DELETE FROM speaker_embeddings
                    WHERE source_segment_id IS NULL
                      AND speaker_id IN ({placeholders})""",
                covered,
            ).rowcount
        self._commit()
        return stale + retired

    def speaker_id_for(self, name: str) -> SpeakerId | None:
        row = self._conn.execute(
            "SELECT id FROM speakers WHERE name = ?", (name,)
        ).fetchone()
        return None if row is None else SpeakerId(int(row["id"]))

    def speaker_profiles(self) -> dict[str, list[list[float]]]:
        """Enrolled people mapped to their reference voiceprints."""
        rows = self._conn.execute(
            """SELECT s.name AS name, e.vector AS vector
               FROM speakers s JOIN speaker_embeddings e ON e.speaker_id = s.id"""
        ).fetchall()
        profiles: dict[str, list[list[float]]] = {}
        for row in rows:
            vector = [float(x) for x in json.loads(row["vector"])]
            profiles.setdefault(str(row["name"]), []).append(vector)
        return profiles

    def corrections(self) -> list[Correction]:
        """All recorded corrections — the labelled fine-tuning corpus."""
        rows = self._conn.execute(
            """SELECT id, audio_segment_id, start_utc, end_utc, corrected_text,
                      language, speaker
               FROM corrections WHERE hidden_reason IS NULL ORDER BY id"""
        ).fetchall()
        return [
            Correction(
                id=CorrectionId(int(row["id"])),
                audio_segment_id=_opt_audio_id(row["audio_segment_id"]),
                start=datetime.fromisoformat(row["start_utc"]),
                end=datetime.fromisoformat(row["end_utc"]),
                corrected_text=str(row["corrected_text"]),
                language=_opt_str(row["language"]),
                speaker=_opt_str(row["speaker"]),
            )
            for row in rows
        ]

    def list_corrections(
        self, *, speaker: str | None = None, limit: int = 200
    ) -> list[LabelledFragment]:
        """Human labels for review, newest first, optionally for one voice."""
        sql = [
            "SELECT id, audio_segment_id, start_utc, end_utc, corrected_text,",
            "speaker, language FROM corrections WHERE hidden_reason IS NULL",
        ]
        params: list[str | int] = []
        if speaker is not None:
            sql.append("AND speaker = ?")
            params.append(speaker)
        sql.append("ORDER BY id DESC LIMIT ?")
        params.append(limit)
        rows = self._conn.execute(" ".join(sql), params).fetchall()
        return [
            LabelledFragment(
                correction_id=CorrectionId(int(row["id"])),
                audio_segment_id=_opt_audio_id(row["audio_segment_id"]),
                start=datetime.fromisoformat(row["start_utc"]),
                end=datetime.fromisoformat(row["end_utc"]),
                text=str(row["corrected_text"]),
                speaker=_opt_str(row["speaker"]),
                language=_opt_str(row["language"]),
            )
            for row in rows
        ]

    def get_correction(self, correction_id: int) -> LabelledFragment | None:
        row = self._conn.execute(
            "SELECT id, audio_segment_id, start_utc, end_utc, corrected_text, "
            "speaker, language FROM corrections WHERE id = ?",
            (correction_id,),
        ).fetchone()
        if row is None:
            return None
        return LabelledFragment(
            correction_id=CorrectionId(int(row["id"])),
            audio_segment_id=_opt_audio_id(row["audio_segment_id"]),
            start=datetime.fromisoformat(row["start_utc"]),
            end=datetime.fromisoformat(row["end_utc"]),
            text=str(row["corrected_text"]),
            speaker=_opt_str(row["speaker"]),
            language=_opt_str(row["language"]),
        )

    def set_correction_speaker(self, correction_id: int, speaker: str) -> None:
        """Re-assign a label's voice: update the corpus pair, the live segment, and
        drop its voiceprint so the backfill re-enrols the clip under the new name.
        """
        row = self._conn.execute(
            "SELECT transcript_segment_id FROM corrections WHERE id = ?",
            (correction_id,),
        ).fetchone()
        self._conn.execute(
            "UPDATE corrections SET speaker = ? WHERE id = ?", (speaker, correction_id)
        )
        if row is not None:
            self._conn.execute(
                """UPDATE transcript_segments SET speaker_label = ?
                   WHERE provenance = ? AND asr_model = ? AND superseded_by IS NULL""",
                (
                    speaker,
                    human_correction_provenance(int(row["transcript_segment_id"])),
                    HUMAN_MODEL,
                ),
            )
        self._conn.execute(
            "DELETE FROM speaker_embeddings WHERE source_correction_id = ?",
            (correction_id,),
        )
        self._commit()

    def nudge_correction(self, correction_id: int, edge: str, delta: float) -> None:
        """Move one boundary of a label by `delta` seconds (signed) — clamped to
        the audio segment and a 0.1s minimum span. Updates the corpus pair, the
        live segment, and drops the voiceprint so it re-enrols from the new span.
        """
        frag = self.get_correction(correction_id)
        if frag is None or frag.audio_segment_id is None:
            return
        seg = self.audio_segment(frag.audio_segment_id)
        if seg is None:
            return
        min_span = timedelta(seconds=0.1)
        start, end = frag.start, frag.end
        shift = timedelta(seconds=delta)
        if edge == "start":
            start = max(seg.start, min(frag.start + shift, end - min_span))
        elif edge == "end":
            end = min(seg.end, max(frag.end + shift, start + min_span))
        else:
            return
        self._conn.execute(
            "UPDATE corrections SET start_utc = ?, end_utc = ? WHERE id = ?",
            (start.isoformat(), end.isoformat(), correction_id),
        )
        row = self._conn.execute(
            "SELECT transcript_segment_id FROM corrections WHERE id = ?",
            (correction_id,),
        ).fetchone()
        if row is not None:
            prov = human_correction_provenance(int(row["transcript_segment_id"]))
            self._conn.execute(
                """UPDATE transcript_segments SET start_utc = ?, end_utc = ?
                   WHERE provenance = ? AND asr_model = ? AND superseded_by IS NULL""",
                (start.isoformat(), end.isoformat(), prov, HUMAN_MODEL),
            )
        self._conn.execute(
            "DELETE FROM speaker_embeddings WHERE source_correction_id = ?",
            (correction_id,),
        )
        self._commit()

    def hide_correction(self, correction_id: int, reason: str) -> None:
        """Soft-remove a bad label from the corpus/counts and drop its voiceprint."""
        self._conn.execute(
            "UPDATE corrections SET hidden_reason = ? WHERE id = ?",
            (reason, correction_id),
        )
        self._conn.execute(
            "DELETE FROM speaker_embeddings WHERE source_correction_id = ?",
            (correction_id,),
        )
        self._commit()


def _opt_float(value: str | int | float | None) -> float | None:
    return None if value is None else float(value)


def _opt_audio_id(value: str | int | float | None) -> AudioSegmentId | None:
    return None if value is None else AudioSegmentId(int(value))


def _opt_speaker_id(value: str | int | float | None) -> SpeakerId | None:
    return None if value is None else SpeakerId(int(value))


def _opt_transcript_id(value: str | int | float | None) -> TranscriptId | None:
    return None if value is None else TranscriptId(int(value))


def _opt_str(value: str | int | float | None) -> str | None:
    return None if value is None else str(value)


def _row_to_segment(row: sqlite3.Row) -> TranscriptSegment:
    return TranscriptSegment(
        id=TranscriptId(int(row["id"])),
        audio_segment_id=_opt_audio_id(row["audio_segment_id"]),
        start=datetime.fromisoformat(row["start_utc"]),
        end=datetime.fromisoformat(row["end_utc"]),
        text=str(row["text"]),
        language=_opt_str(row["language"]),
        language_confidence=_opt_float(row["language_confidence"]),
        asr_confidence=_opt_float(row["asr_confidence"]),
        asr_model=str(row["asr_model"]),
        speaker_label=_opt_str(row["speaker_label"]),
        speaker_id=_opt_speaker_id(row["speaker_id"]),
        superseded_by=_opt_transcript_id(row["superseded_by"]),
        created=_opt_dt(row["created_utc"]),
        provenance=_opt_str(row["provenance"]),
        hidden_reason=_opt_str(row["hidden_reason"]),
        loudness=_opt_float(row["loudness"]),
        speaker_guess=_opt_str(row["speaker_guess"]),
        speaker_score=_opt_float(row["speaker_score"]),
        speaker_cluster=_opt_str(row["speaker_cluster"]),
        # Present only when the query joins it in (e.g. recent_transcripts); other
        # callers select transcript_segments alone, so default to None.
        source_id=(
            # `in` on a sqlite3.Row checks values, not keys, so .keys() is right.
            _opt_str(row["source_id"])
            if "source_id" in row.keys()  # noqa: SIM118
            else None
        ),
        word_timings=_load_word_timings(row["word_timings"]),
    )


def _rebase_word_timings(
    words: Sequence[Word] | None, *, shift: float, duration: float
) -> list[Word] | None:
    """Shift word offsets by `shift` seconds and clip to [0, duration].

    Words entirely outside the new span are dropped; boundary words are clipped.
    None stays None (turns without timings are untouched).
    """
    if words is None:
        return None
    rebased = [
        Word(
            start=max(0.0, w.start + shift),
            end=min(duration, w.end + shift),
            text=w.text,
            probability=w.probability,
        )
        for w in words
        if w.end + shift > 0 and w.start + shift < duration
    ]
    return rebased


def _dump_word_timings(words: Sequence[Word] | None) -> str | None:
    """Serialize per-word timings to JSON ({s,e,w} per word). None/empty → NULL."""
    if not words:
        return None
    return json.dumps([{"s": w.start, "e": w.end, "w": w.text} for w in words])


def _load_word_timings(value: str | int | float | None) -> tuple[Word, ...] | None:
    if not isinstance(value, str):
        return None
    return tuple(
        Word(start=float(d["s"]), end=float(d["e"]), text=str(d["w"]), probability=1.0)
        for d in json.loads(value)
    )


def _opt_dt(value: str | int | float | None) -> datetime | None:
    return None if value is None else datetime.fromisoformat(str(value))


def _opt_int(value: str | int | float | None) -> int | None:
    return None if value is None else int(value)


# The ab_compare_runs columns common to every read (result_json is appended only by
# get_ab_compare_run, which sets with_result=True).
_AB_COMPARE_COLS = (
    "id, source_id, start_utc, end_utc, model_a, model_b, base_model, status, "
    "created_utc, started_utc, done_utc, error, mean_wer_a, mean_wer_b, "
    "n_corrections, n_segments, n_changed"
)


def _row_to_ab_compare_job(row: sqlite3.Row, *, with_result: bool) -> AbCompareJob:
    return AbCompareJob(
        id=int(row["id"]),
        source=str(row["source_id"]),
        start=_opt_dt(row["start_utc"]),
        end=_opt_dt(row["end_utc"]),
        model_a=str(row["model_a"]),
        model_b=str(row["model_b"]),
        base_model=str(row["base_model"]),
        status=str(row["status"]),
        created=datetime.fromisoformat(str(row["created_utc"])),
        started=_opt_dt(row["started_utc"]),
        done=_opt_dt(row["done_utc"]),
        error=_opt_str(row["error"]),
        result_json=_opt_str(row["result_json"]) if with_result else None,
        mean_wer_a=_opt_float(row["mean_wer_a"]),
        mean_wer_b=_opt_float(row["mean_wer_b"]),
        n_corrections=_opt_int(row["n_corrections"]),
        n_segments=_opt_int(row["n_segments"]),
        n_changed=_opt_int(row["n_changed"]),
    )
