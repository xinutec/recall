"""Searchable, versioned transcript store (SQLite + FTS5).

The store is the backbone of the memory aid. It holds:

- `sources` / `audio_segments` — the retained raw-audio index (source of truth).
- `transcript_segments` — derived, versioned views: each carries the model and
  confidences that produced it, and is *superseded* (never deleted) when a
  better pass replaces it. This is the "never commit, always re-derivable" model
  from pipeline.md §6.
- `transcript_fts` — FTS5 full-text index over transcript text.

Search and time-range queries return only *current* (non-superseded) segments.

⚠ **This class is SHRINKING, and what is left is not all live.** The browsing
reads moved to `recalld` and the terminal to `recall-cli`, so on 2026-09-15 nine
methods with no production caller were deleted with their tests (`recent_transcripts`,
`session_summaries`, `supersede_many`, `current_version`, `set_speaker_guess`,
`mark_unreadable_capture`, `unreadable_capture_names`, `audio_segment_id_at`,
`_count`). On 2026-09-17 `refine.py` and `jobs.py` went, taking `add_refine_request`,
`pending_refine_requests`, `mark_refine_request_done`, `audio_segments_to_diarize`,
`audio_segments_to_rediarize`, `audio_segments_in_range`, the four diarize-skip
helpers and `rollback` — whose only caller was the refine daemon's per-pass recovery.
The rest have no production caller either and were KEPT, each for a reason worth not
rediscovering:

- `schema_version` is the only accessor four MIGRATION tests assert through, and
  the migration machinery is live — `open()` calls `migrate()`.
- `delete_source`, `delete_audio_segments` and `is_tombstoned` are the local
  delete path AND its tombstone journal. The capability moved to the fleet; the
  journal is what stops a deleted identity being resurrected by the next push,
  so removing the guard along with the caller is how it comes back without it.
- `memory` and `segments_in_range` are how eleven and seven other test files
  respectively construct their world.
- `set_turn_speaker`, `rename_source`, `add_capture_event`, `set_loudness`,
  `set_audio_analysis`, `set_audio_measurement`, `source_kind`, `sources_of`,
  `correction_count` and `pending_audio_segments` set up tests for rules that ARE
  live. ⚠ `is_diarize_skipped` and `diarize_skip_reason` were on that list until
  2026-09-17 and are now gone with the rest of the diarize-skip guard: the rule
  they set tests up for stopped being live when `refine.py` was deleted, and no
  Rust reads `diarize_skips`. A reason to keep something expires with the thing
  it points at.

So "no production caller" is where that question starts, not where it ends.
"""

from __future__ import annotations

import json
import sqlite3
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path
from typing import Self

from recall.asr import Word
from recall.capture_control import CaptureEventKind
from recall.ids import AudioSegmentId, CorrectionId, SpeakerId, TranscriptId
from recall.sources import AudioSource, SourceKind, SourceRow
from recall.store_models import (
    CaptureEvent,
    ClusterNaming,
    Correction,
    LabelledFragment,
    PendingVoiceprint,
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
    "HUMAN_MODEL",
    "LIVE_MODEL",
    "REPROCESSED_MARKER",
    "SCHEMA_VERSION",
    "_MIGRATIONS",
    "CaptureEvent",
    "ClusterNaming",
    "Correction",
    "LabelledFragment",
    "PendingVoiceprint",
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


# Hidden-reason / provenance prefixes marking a turn produced by a re-derive pass.
# The resumable query (_segments_without_marker), redrive.py, recalld's diarized
# writer and the UI tier check all key off these prefixes, so
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

    Written by review.apply_correction and MATCHED by set_correction_speaker to
    find that live turn again — one function, so the writer and the matcher can
    never drift apart.
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
            "SELECT id, path, start_utc FROM audio_segments WHERE source_id = ?",
            (source_id,),
        ).fetchall()
        audio_ids = [int(r["id"]) for r in seg_rows]
        paths = [str(r["path"]) for r in seg_rows]
        with self.transaction():
            for r in seg_rows:
                # Journaled so the deletion crosses the split: the Mac removes its
                # copies, and a later refine push can't resurrect the session here.
                self._tombstone(source_id, str(r["start_utc"]))
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

    def _tombstone(self, source_id: str, start_utc: str) -> None:
        """Journal one deliberate segment deletion by cross-machine identity, inside
        the caller's transaction — the veto that stops a later push resurrecting the
        segment. It is a record, never an order: nothing serves it to a recorder.
        OR IGNORE: deleting twice is one fact."""
        self._conn.execute(
            "INSERT OR IGNORE INTO deleted_segments "
            "(source_id, start_utc, deleted_utc) VALUES (?, ?, ?)",
            (source_id, start_utc, datetime.now(UTC).isoformat()),
        )

    def delete_audio_segments(self, audio_ids: Sequence[AudioSegmentId]) -> list[str]:
        """Hard-delete specific capture segments and all derived from them (turns and
        their lineage/embeddings/corrections/FTS), returning the audio file paths to
        unlink. For the quiet-cleanup: a human-confirmed span of total-quiet capture is
        truly removed to reclaim disk. Atomic. Each deletion is journaled as a
        tombstone (see `_tombstone`) so it propagates across the Isis split."""
        paths: list[str] = []
        with self.transaction():
            for audio_id in audio_ids:
                row = self._conn.execute(
                    "SELECT path, source_id, start_utc FROM audio_segments "
                    "WHERE id = ?",
                    (int(audio_id),),
                ).fetchone()
                if row is None:
                    continue
                paths.append(str(row["path"]))
                self._tombstone(str(row["source_id"]), str(row["start_utc"]))
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
        """Authoritative registration by whoever knows the source's true kind — the
        capture agent (coreaudio), the ingest handshake (tcp_pcm), an upload. Corrects
        the DISCOVERED kind the worker registers when it finds a directory of audio
        with no source row.

        The name is preserved *unless* it is still the worker's placeholder (equal to
        the id): a name the user chose in the UI is theirs and re-registering a phone
        must never rename it back, but "meeting-20260731-0916" is nobody's choice and
        the registrar that knows this is a meeting may say so.
        """
        self._conn.execute(
            """INSERT INTO sources (id, name, kind, port) VALUES (?, ?, ?, ?)
               ON CONFLICT(id) DO UPDATE SET
                   kind = excluded.kind,
                   port = excluded.port,
                   name = CASE WHEN sources.name = sources.id
                               THEN excluded.name ELSE sources.name END""",
            (source.id, source.name, source.kind.value, source.port),
        )
        self._commit()

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

    def hide(self, transcript_id: int, reason: str) -> None:
        """Soft-hide a turn (e.g. a confirmed hallucination) with a reason.

        It leaves all current views but is never deleted — fully recoverable.
        """
        self._conn.execute(
            "UPDATE transcript_segments SET hidden_reason = ? WHERE id = ?",
            (reason, transcript_id),
        )
        self._commit()

    def unhide(self, transcript_id: int) -> None:
        """Restore a soft-hidden turn (recover a false-positive hide)."""
        self._conn.execute(
            "UPDATE transcript_segments SET hidden_reason = NULL WHERE id = ?",
            (transcript_id,),
        )
        self._commit()

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
        self, marker: str, *, limit: int
    ) -> list[AudioSegmentId]:
        """Audio segments that still have visible machine turns but no turn yet
        hidden with `marker` (a 'reason' prefix), oldest-first. The hidden-turn
        marker is what makes a re-derive pass resumable + chunkable.

        It took three optional orderings — newest-first, a diarize-skip exclusion
        and a speech weighting — for the refine daemon's pickers. All three went
        with `refine.py` on 2026-09-17; `redrive` is the only caller left and wants
        none of them.
        """
        rows = self._conn.execute(
            "SELECT DISTINCT audio_segment_id FROM transcript_segments "
            "WHERE audio_segment_id IS NOT NULL AND superseded_by IS NULL "
            "AND hidden_reason IS NULL AND asr_model != ? "
            "AND audio_segment_id NOT IN ("
            "  SELECT audio_segment_id FROM transcript_segments "
            "  WHERE hidden_reason LIKE ? AND audio_segment_id IS NOT NULL) "
            "ORDER BY audio_segment_id ASC LIMIT ?",
            (HUMAN_MODEL, marker + "%", limit),
        ).fetchall()
        return [AudioSegmentId(int(r["audio_segment_id"])) for r in rows]

    def audio_segments_to_redrive(self, *, limit: int) -> list[AudioSegmentId]:
        """Segments still needing the basic re-derive (no 'reprocessed' marker)."""
        return self._segments_without_marker(REPROCESSED_MARKER, limit=limit)

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

    def unmirrored_segments(
        self, *, limit: int = 500, older_than: datetime | None = None
    ) -> list[AudioSegmentId]:
        """Processed segments that have never reached the fleet (`pushed_utc` unset),
        oldest-first — the mirror-completion queue. Covers what the turn-watermark
        push cannot: a speechless segment mints no turn ids, so it never synced and
        the fleet's quiet review could never sweep it. `older_than` filters to
        segments processed before that time (the doctor's in-flight slack)."""
        clause = "WHERE transcribed_utc IS NOT NULL AND pushed_utc IS NULL"
        args: list[object] = []
        if older_than is not None:
            clause += " AND transcribed_utc < ?"
            args.append(older_than.isoformat())
        rows = self._conn.execute(
            f"SELECT id FROM audio_segments {clause} ORDER BY id LIMIT ?",
            (*args, limit),
        ).fetchall()
        return [AudioSegmentId(int(r["id"])) for r in rows]

    def mark_pushed(self, audio_id: AudioSegmentId) -> None:
        """Stamp that this segment (audio + current turns) reached the fleet."""
        self._conn.execute(
            "UPDATE audio_segments SET pushed_utc = ? WHERE id = ?",
            (datetime.now(UTC).isoformat(), int(audio_id)),
        )
        self._commit()

    def is_tombstoned(self, source: str, start: datetime) -> bool:
        """Whether this identity was deliberately deleted here — the veto that stops
        a later sync push resurrecting it on the fleet.

        This is the whole of what a deletion travels as now. It refuses a re-push; it
        does not ask the Mac to delete its own copy, and there is no job that does
        (docs/architecture.md, "Deletion authority")."""
        row = self._conn.execute(
            "SELECT 1 FROM deleted_segments WHERE source_id = ? AND start_utc = ?",
            (source, start.isoformat()),
        ).fetchone()
        return row is not None

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

    def vocabulary_terms(self) -> list[VocabularyTerm]:
        rows = self._conn.execute(
            "SELECT id, term FROM vocabulary ORDER BY term COLLATE NOCASE"
        ).fetchall()
        return [VocabularyTerm(id=int(r["id"]), term=str(r["term"])) for r in rows]

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

    def get_transcript(self, segment_id: int) -> TranscriptSegment | None:
        row = self._conn.execute(
            "SELECT * FROM transcript_segments WHERE id = ?", (segment_id,)
        ).fetchone()
        return None if row is None else _row_to_segment(row)

    def set_loudness(self, segment_id: int, value: float) -> None:
        """Persist a turn's measured loudness (speech_level) so the labeling queue
        can rank by it without re-decoding the audio on the request path.
        """
        self._conn.execute(
            "UPDATE transcript_segments SET loudness = ? WHERE id = ?",
            (value, segment_id),
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

        The authoritative human naming of a voice; it overrides the auto guess. It
        writes no correction, but it is not display-only: `speaker_label` is what
        `turns_needing_voiceprint` selects on, so the backfill enrols this voice from
        its turns — naming a meeting's clinician does add them to the matching pool,
        the same as any household voice.

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

    def cluster_namings(self) -> list[ClusterNaming]:
        """Every human naming of a voice, as (source, cluster, name) — the whole set,
        so it is the fleet→Mac label channel's payload and the Mac's own diff baseline.

        A human name lives denormalized on `speaker_label`; the cluster is the shared
        key both machines carry (it rides every segment push). A cluster is one voice,
        so if a few of its turns were individually reassigned to different names, the
        cluster's *dominant* (most-turns) label wins — a single mapping per voice, which
        is exactly what `name_voice` replays on the Mac. Ordered for a stable payload.
        """
        rows = self._conn.execute(
            "SELECT a.source_id src, ts.speaker_cluster cl, ts.speaker_label lbl, "
            "COUNT(*) n FROM transcript_segments ts "
            "JOIN audio_segments a ON a.id = ts.audio_segment_id "
            "WHERE ts.speaker_label IS NOT NULL AND ts.speaker_cluster IS NOT NULL "
            "AND ts.superseded_by IS NULL AND ts.hidden_reason IS NULL "
            "GROUP BY a.source_id, ts.speaker_cluster, ts.speaker_label "
            "ORDER BY a.source_id, ts.speaker_cluster, n DESC, ts.speaker_label"
        ).fetchall()
        dominant: dict[tuple[str, str], str] = {}
        for r in rows:
            key = (str(r["src"]), str(r["cl"]))
            if (
                key not in dominant
            ):  # first row per (src, cl) is the highest-count label
                dominant[key] = str(r["lbl"])
        return [
            ClusterNaming(source_id=src, cluster=cl, name=name)
            for (src, cl), name in dominant.items()
        ]

    def set_turn_speaker(self, segment_id: int, name: str | None) -> None:
        """Set/clear the human speaker label on one turn — reassign a mis-diarized turn
        to the right voice. Display label only (no correction/voiceprint)."""
        self._conn.execute(
            "UPDATE transcript_segments SET speaker_label = ? WHERE id = ?",
            (name, segment_id),
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
        Unknown loudness (not yet measured) is kept.

        ⚠ **The loudness half of this gate is INERT, and has been since the API
        moved.** `set_loudness` is the only writer of that column and nothing in
        production calls it any more — only tests do — so every row reads NULL
        and the `IS NULL` arm keeps all of them. The default is the right one for
        "not yet measured", which is exactly why the gate went quiet instead of
        failing. What still bites is the `min_seconds` half, which is measured.
        Do not read a passing enrolment as evidence that near-silent clips were
        excluded. Gating touches enrolment only — the
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
        # Present only when the query LEFT JOINs audio_segments in; other callers
        # select transcript_segments alone, so default to None.
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
