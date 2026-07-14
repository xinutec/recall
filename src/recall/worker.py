"""Background transcription worker: turn newly-captured audio into transcripts.

Runs periodically (launchd). Each pass indexes any new segment files, then
transcribes the ones that don't have a transcript yet — diarized when a diarizer
is supplied (per-turn language + speaker turns), otherwise whole-clip. The
in-progress segment (still being written by capture) is skipped via a min-age
guard. Idempotent: a transcribed segment is never redone.
"""

from __future__ import annotations

import logging
import os
import time
from pathlib import Path

from recall.asr import Transcriber
from recall.diarize import Diarizer
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.probe import Scan, scan_source
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.vad import Vad

_log = logging.getLogger("recall.worker")

DEFAULT_MIN_AGE_S = 120.0

# subdirectories under the data root that are not audio sources
_NON_SOURCE_DIRS = {"work"}


def reconcile_live(store: Store) -> int:
    """Hide provisional live transcripts the archive pass has caught up to.

    A live turn is hidden once a transcribed audio segment actually spans its
    moment — not merely because some later archive turn exists (a watermark would
    hide live turns in never-recorded gaps, e.g. empty-start segments cleared as
    dead stubs, losing the only record of that stretch). Hidden — NOT superseded:
    `superseded_by` means "a better version of this same utterance", and there is
    no single archive turn that is that; pointing it at an arbitrary one made deep
    links resolve to unrelated text. Returns how many.
    """
    return store.hide_provisional_covered()


def _old_enough(path: Path, current: float, min_age_seconds: float) -> bool:
    """True if `path` exists and hasn't been touched within `min_age_seconds`.

    A pending row whose file has vanished must not raise — one bad row would
    crash-loop the whole worker (and the pause auto-resume that rides on it).
    """
    try:
        return current - path.stat().st_mtime >= min_age_seconds
    except FileNotFoundError:
        return False


def discover_source_ids(root: Path) -> list[str]:
    """Source ids = subdirectories that actually hold segment files
    (`<id>-<timestamp>.<ext>`).

    Requiring a segment file — not just any directory — means non-source dirs under
    the data root (the refine `work` dir, fine-tune pilot output like `pilot-finetune`
    with its `clips/` + manifests, etc.) are never mistaken for recorders and
    registered as phantom sources.
    """
    if not root.is_dir():
        return []
    ids: list[str] = []
    for entry in root.iterdir():
        if not entry.is_dir() or entry.name in _NON_SOURCE_DIRS:
            continue
        prefix = f"{entry.name}-"
        with os.scandir(entry) as files:
            if any(f.is_file() and f.name.startswith(prefix) for f in files):
                ids.append(entry.name)
    return sorted(ids)


def _clear_dead_stubs(scan: Scan) -> None:
    """Remove the zero-byte files capture left behind, and *say* it — loudly.

    A zero-byte capture file holds no audio and never will: a segment path carries its
    own timestamp, so capture never reopens one. It is a tombstone marking the instant
    capture died. Left in place it is not harmless — the indexer re-probed 46 of them on
    every pass for three weeks, and, being unindexed, they were invisible to every check
    the archive has.

    So they go, and each one is logged with its time. That log line is the *only*
    durable record that capture failed at that moment: the timeline gap says audio is
    missing, but not that anything went wrong. A file that is unreadable but NOT empty
    may still hold audio, so it is only reported, never removed.
    """
    for path in scan.unreadable:
        _log.warning(
            "unreadable capture file (kept — it may still hold audio): %s", path
        )
    for path in scan.empty:
        try:
            path.unlink()
        except OSError as err:  # read-only volume, vanished file: report, don't crash
            _log.warning("could not remove empty capture file %s: %s", path, err)
            continue
        _log.warning("capture died here — removed empty file: %s", path.name)


def process_pending(  # noqa: PLR0913 - pipeline collaborators + tuning knobs
    store: Store,
    root: Path,
    source: AudioSource,
    transcriber: Transcriber,
    *,
    model_name: str,
    diarizer: Diarizer | None = None,
    vad: Vad | None = None,
    min_age_seconds: float = DEFAULT_MIN_AGE_S,
    now: float | None = None,
) -> int:
    """Index new audio under `root/<source.id>/` and transcribe what's pending.

    Returns the number of transcript rows written. Segments modified within
    `min_age_seconds` are skipped (still being recorded). A `vad` gates the
    no-diarizer path so silence isn't transcribed (no Whisper hallucinations).
    """
    current = time.time() if now is None else now
    store.add_source(source)

    audio_dir = root / source.id
    # Only probe files we haven't indexed yet — probing decodes the whole file,
    # so re-scanning the entire archive each pass would grow unbounded. The min-age
    # guard applies here at INDEX time too: a partial file probes fine but yields
    # a truncated duration that would be recorded permanently.
    known = frozenset(path for _, path in store.audio_segment_paths())
    scan = scan_source(
        audio_dir, source.id, known=known, min_age_seconds=min_age_seconds, now=current
    )
    for segment in scan.segments:
        store.add_audio_segment(segment)
    _clear_dead_stubs(scan)

    pending = [
        segment
        for segment in store.pending_audio_segments()
        if segment.source_id == source.id
        and _old_enough(Path(segment.path), current, min_age_seconds)
    ]
    if not pending:
        return 0

    work_dir = root / "work"
    if diarizer is not None:
        return ingest_diarized(
            store,
            pending,
            diarizer,
            transcriber,
            work_dir=work_dir,
            model_name=model_name,
        )
    return ingest_transcripts(
        store, pending, transcriber, work_dir=work_dir, model_name=model_name, vad=vad
    )


def process_all(  # noqa: PLR0913 - pipeline collaborators + tuning knobs
    store: Store,
    root: Path,
    transcriber: Transcriber,
    *,
    model_name: str,
    diarizer: Diarizer | None = None,
    vad: Vad | None = None,
    min_age_seconds: float = DEFAULT_MIN_AGE_S,
    now: float | None = None,
) -> int:
    """Transcribe pending audio across every source under `root`."""
    total = 0
    for source_id in discover_source_ids(root):
        source = AudioSource(
            id=source_id, name=source_id, kind=SourceKind.COREAUDIO, spec=""
        )
        total += process_pending(
            store,
            root,
            source,
            transcriber,
            model_name=model_name,
            diarizer=diarizer,
            vad=vad,
            min_age_seconds=min_age_seconds,
            now=now,
        )
    return total
