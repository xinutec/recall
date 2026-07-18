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

from recall import capture_control
from recall.asr import Transcriber
from recall.capture import parse_segment_start, segment_glob
from recall.diarize import Diarizer
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.probe import Scan, scan_source
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.vad import Vad

_log = logging.getLogger("recall.worker")

DEFAULT_MIN_AGE_S = 120.0

# An UNREADABLE capture file at or below this size is a header-only dead-capture
# tombstone — capture opened the segment and died before writing any audio pages
# (observed ~136 bytes for Opus). It holds nothing the pipeline can use (ffprobe refuses
# it), so it is removed like a 0-byte file. A LARGER unreadable file might hold a
# recoverable audio body behind a corrupt header, so it is kept. Only unreadable files
# are measured here — a readable short segment decodes fine and never reaches this.
_DEAD_CAPTURE_MAX_BYTES = 1024

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


def _is_dead_capture(path: Path) -> bool:
    """True if an unreadable file is small enough to be a header-only tombstone (no room
    for usable audio) rather than a larger corrupt file that might hold a recoverable
    audio body. A vanished file counts as nothing to remove."""
    try:
        return path.stat().st_size <= _DEAD_CAPTURE_MAX_BYTES
    except OSError:
        return False


def _clear_dead_stubs(scan: Scan, store: Store, source_id: str) -> None:
    """Remove the dead-capture files capture left behind, and *record* each — durably.

    A dead-capture file holds no usable audio and marks the instant capture died: a
    zero-byte file (capture opened it and wrote nothing), or a tiny truncated one (a
    header with no audio pages — ffprobe refuses it; observed ~136 bytes for Opus). Both
    are removed. Left in place they were re-probed on every pass forever and, being
    unindexed, were invisible to every check the archive has.

    The NEWEST file is exempt, whatever its age: ffmpeg writes a segment's bytes only
    when it closes (measured live 2026-07-16 — the current segment sat at 0 bytes,
    held open, for 3 minutes while lsof showed ffmpeg's open fd). Unlinking it would
    send that eventual flush to a deleted inode: silent, unrecoverable loss — the very
    thing this bookkeeping exists to prevent.

    Before each unlink a durable `dead_window` capture-event is written, timestamped to
    the dead segment's own moment, so a later gap check tells this apart from a
    deliberate pause. A LARGER unreadable file may hold a recoverable audio body behind
    a corrupt header, so it is NOT removed — only recorded once so the scan stops
    re-probing it (see `unreadable_captures`).
    """
    tombstones = list(scan.empty)
    for path in scan.unreadable:
        if _is_dead_capture(path):
            tombstones.append(path)  # header-only — removed below, like a 0-byte file
        elif store.mark_unreadable_capture(source_id, path.name):
            # Too big to be header-only: keep it (may hold audio), record once so the
            # next scan skips it instead of re-probing + re-logging every pass.
            _log.warning(
                "unreadable capture file (kept, recorded — won't re-probe): %s", path
            )

    if not tombstones:
        return
    files = segment_glob(tombstones[0].parent, source_id)
    newest = files[-1].name if files else None
    for path in tombstones:
        if path.name == newest:
            continue  # possibly ffmpeg's open segment — see the docstring
        # Record the death BEFORE unlinking — the file is about to be gone, so this
        # event becomes the evidence. utc is the segment's own timestamp (death time).
        try:
            store.add_capture_event(
                capture_control.CaptureEventKind.DEAD_WINDOW,
                utc=parse_segment_start(path.name),
                source_id=source_id,
                detail=path.name,
            )
        except Exception:  # never let bookkeeping block cleanup
            _log.exception("could not record dead-window event for %s", path.name)
        try:
            path.unlink()
        except OSError as err:  # read-only volume, vanished file: report, don't crash
            _log.warning("could not remove dead capture file %s: %s", path, err)
            continue
        _log.warning("capture died here — removed dead file: %s", path.name)


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
    known = frozenset(path for _, path in store.audio_segment_paths()) | frozenset(
        # Files already found unreadable are kept but never re-probed (see v42) —
        # without this they re-probed and re-logged every pass forever.
        str(audio_dir / name)
        for name in store.unreadable_capture_names(source.id)
    )
    scan = scan_source(
        audio_dir, source.id, known=known, min_age_seconds=min_age_seconds, now=current
    )
    for segment in scan.segments:
        store.add_audio_segment(segment)
    _clear_dead_stubs(scan, store, source.id)

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
