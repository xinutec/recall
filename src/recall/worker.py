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
from datetime import datetime, timedelta
from pathlib import Path

from recall import capture_control
from recall.asr import Transcriber
from recall.capture import CaptureConfig, parse_segment_start, segment_glob
from recall.diarize import Diarizer
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.probe import Scan, scan_source
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.vad import Vad

_log = logging.getLogger("recall.worker")

DEFAULT_MIN_AGE_S = 120.0

# Segments one source may transcribe per pass before the others get their turn.
# Sized so a busy pass still cycles all four mics within a few minutes: a quiet
# segment is VAD-skipped in milliseconds and a speech-bearing minute costs a few
# seconds, so 20 is minutes of work, not hours. None disables the cap.
DEFAULT_MAX_TRANSCRIBE_PER_SOURCE = 20

# An UNREADABLE capture file at or below this size is a header-only dead-capture
# tombstone — capture opened the segment and died before writing any audio pages
# (observed ~136 bytes for Opus). It holds nothing the pipeline can use (ffprobe refuses
# it), so it is removed like a 0-byte file. A LARGER unreadable file might hold a
# recoverable audio body behind a corrupt header, so it is kept. Only unreadable files
# are measured here — a readable short segment decodes fine and never reaches this.
_DEAD_CAPTURE_MAX_BYTES = 1024

# A stub covers its own segment window: from its start until the next rotation would
# have closed it. A pause inside that window is what killed it.
_SEGMENT_SPAN = timedelta(seconds=CaptureConfig().segment_seconds)
# ffmpeg opens the segment and capture-control writes the pause from different places;
# a pause recorded a beat before the file appeared is still the same act.
_PAUSE_SLACK = timedelta(seconds=5)

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


def _pause_explains(store: Store, start: datetime) -> bool:
    """Was this stub cut short by a deliberate pause rather than a dead device?

    Turning capture off mid-segment leaves behind exactly what a dead device leaves.
    ffmpeg writes a segment's bytes only when it closes, so a segment killed seconds
    after it opened is a header-only stub — indistinguishable, on disk, from a mic that
    delivered digital silence. Recording that as a dead window is wrong twice over: no
    speech was lost, and it hard-fails the loss check for 48 hours over something the
    household did on purpose. The pause is already the evidence; a second event
    contradicting it helps nobody.

    A pause is a global act (capture_control.pause writes one file and every recording
    agent parks itself), so it is not matched per source.
    """
    pauses = store.capture_events_since(
        start - _PAUSE_SLACK, kinds=(capture_control.CaptureEventKind.PAUSE,)
    )
    return any(event.utc <= start + _SEGMENT_SPAN for event in pauses)


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
        # Unless a deliberate pause already explains it, in which case there is no
        # death to record: see _pause_explains.
        try:
            start = parse_segment_start(path.name)
            if _pause_explains(store, start):
                _log.info(
                    "dead stub cut short by a deliberate pause, not lost speech: %s",
                    path.name,
                )
            else:
                store.add_capture_event(
                    capture_control.CaptureEventKind.DEAD_WINDOW,
                    utc=start,
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
    max_transcribe: int | None = None,
) -> int:
    """Index new audio under `root/<source.id>/` and transcribe what's pending.

    Returns the number of transcript rows written. Segments modified within
    `min_age_seconds` are skipped (still being recorded). A `vad` gates the
    no-diarizer path so silence isn't transcribed (no Whisper hallucinations).

    `max_transcribe` caps how many segments this call transcribes (indexing is
    never capped — see process_all). None means "all of them", the single-source
    behaviour.
    """
    index_source(store, root, source, min_age_seconds=min_age_seconds, now=now)
    return transcribe_pending(
        store,
        root,
        source,
        transcriber,
        model_name=model_name,
        diarizer=diarizer,
        vad=vad,
        min_age_seconds=min_age_seconds,
        now=now,
        max_transcribe=max_transcribe,
    )


def index_source(
    store: Store,
    root: Path,
    source: AudioSource,
    *,
    min_age_seconds: float = DEFAULT_MIN_AGE_S,
    now: float | None = None,
) -> None:
    """Index new audio under `root/<source.id>/` — the CHEAP half (ffprobe only).

    Separated from transcription because the two have wildly different costs and
    the timeline only needs this one. Indexing every source before any Whisper
    runs is what stops the alphabetically-last mic waiting a whole cycle for its
    rows (#1365: usb sorts last of 33 source dirs and sat two hours behind
    during a visit, its audio safe on disk but unsearchable).
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


def transcribe_pending(  # noqa: PLR0913 - pipeline collaborators + tuning knobs
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
    max_transcribe: int | None = None,
) -> int:
    """Transcribe this source's already-indexed pending audio — the EXPENSIVE half.

    `max_transcribe` bounds one call so the sources share the budget rather than
    the first draining it. Oldest first, so a capped pass always advances the
    front of the queue.
    """
    current = time.time() if now is None else now
    pending = [
        segment
        for segment in store.pending_audio_segments()
        if segment.source_id == source.id
        and _old_enough(Path(segment.path), current, min_age_seconds)
    ]
    if max_transcribe is not None:
        # Oldest first (pending_audio_segments is ordered), so a capped pass always
        # advances the front of the queue rather than skimming whatever is newest.
        pending = pending[:max_transcribe]
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
    max_transcribe_per_source: int | None = DEFAULT_MAX_TRANSCRIBE_PER_SOURCE,
) -> int:
    """Transcribe pending audio across every source under `root`, fairly.

    Sources used to be served in sorted order, each drained completely before the
    next — so under multi-mic load the alphabetically-later mics starved. Measured
    live during a visit (2026-09-03): iphone11 current, usb two hours unindexed,
    pixel5 three. No audio was lost — it sat on disk, unsearchable, which is its
    own kind of loss while the conversation is still happening.

    So a pass now gives every source a bounded slice of the expensive work
    (`max_transcribe_per_source`) instead of letting the first take it all. The
    reconciler reads audio_segment rows, so this also stops worker lag reading as
    a recording gap in the doctor.
    """
    # DISCOVERED, not COREAUDIO: a directory of audio says nothing about what
    # recorded it, and `add_source` is INSERT OR IGNORE — so a guess made here is
    # permanent unless the real registrar can correct it (Store.register_source).
    sources = [
        AudioSource(id=sid, name=sid, kind=SourceKind.DISCOVERED, spec="")
        for sid in discover_source_ids(root)
    ]
    # Phase 1 — index EVERY source. Cheap (ffprobe), and it is what the timeline,
    # the loss reconciler and the doctor all read, so no mic should wait behind
    # another mic's Whisper time for it.
    for source in sources:
        index_source(store, root, source, min_age_seconds=min_age_seconds, now=now)
    # Phase 2 — spend the expensive budget, a bounded slice each.
    total = 0
    for source in sources:
        total += transcribe_pending(
            store,
            root,
            source,
            transcriber,
            model_name=model_name,
            diarizer=diarizer,
            vad=vad,
            min_age_seconds=min_age_seconds,
            now=now,
            max_transcribe=max_transcribe_per_source,
        )
    return total
