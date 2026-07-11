"""Find long total-quiet spans in the continuous capture — the mic's noise floor, no
speech — so they can be reviewed and deleted (most of the archive is pure waste).

The USB mic emits a consistent noise floor with little variation, so quiet separates
from speech cleanly by RAW mean volume: measured on real capture, quiet segments cluster
near -62 dB (within 1 dB) while any sound sits above ~-55 dB. NB: the DB's `loudness`
column is useless here — it's post-loudnorm, which flattens the gap; the signal is the
raw mean volume of the untouched Opus.

Volume alone is not enough to call a minute empty, and deleting a capture segment takes
everything derived from it — its turns, their corrections, their lineage. So four rules
keep a proposed span from ever meaning anything other than "this one mic heard nothing,
continuously, for this long":

* **Speech outranks volume.** A segment with a current, visible turn — machine or
  human — is never quiet, whatever its mean says. Measured on real capture, far-field
  Dutch sentences and a *human-corrected* turn sit on segments whose 60-second mean is
  under -60 dB: a few seconds of quiet speech barely move a minute's average. Volume is
  a candidate generator; the transcript is the evidence, and it wins.
* **Only what ASR has already examined.** An untranscribed segment is unknown, not
  empty, and nothing unread is ever swept.
* **Per source.** Several mics record the same room at once (the USB mic and the
  phones), so segments interleave in time. A run is grouped *within* a source; mixing
  them would put one mic's files in another's span, and a span would then delete audio
  it never showed you.
* **Uploads are never swept, and a gap breaks a run.** An UPLOAD source is a purposely
  imported recording (a meeting), never idle room noise. And if capture stopped, the
  missing time is unknown, not quiet — without that rule a 4-minute hole could carry a
  run past the 5-minute bar on its own.

What survives all four is still only a *proposal*: the review UI draws the waveform (see
`recall.envelope`), because a 60-second mean also hides brief sounds — a bump, a cough —
that a human should hear before agreeing to lose them. Deletion is always confirmed, and
its boundaries corrected, by a person; nothing here is automatic.
"""

from __future__ import annotations

import re
import subprocess
from collections import defaultdict
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from recall.ids import AudioSegmentId
from recall.sources import SourceKind
from recall.store import SegmentVolume, Store

# Between the ~-62 dB noise floor and the ~-55 dB quietest real sound (measured). A
# segment at/under this is the mic idling; above it, something happened. Necessary but
# not sufficient — see `is_quiet`.
QUIET_MEAN_DB = -60.0
# Only long runs are worth surfacing — a few seconds of quiet between utterances is
# normal speech rhythm, not waste. Default: 5 minutes.
MIN_QUIET_SPAN_S = 300.0
# Back-to-back capture segments abut within milliseconds; anything larger is a hole in
# the recording, which is unknown, not quiet, and so must break the run.
MAX_GAP_S = 2.0
# Continuous capture only. UPLOAD is an imported recording (a meeting) — the archive's
# most valuable audio, never a stream of idle noise, so never a delete candidate.
SWEEPABLE_KINDS = frozenset(SourceKind) - {SourceKind.UPLOAD}

_MEAN_VOLUME = re.compile(r"mean_volume:\s*(-?\d+(?:\.\d+)?)\s*dB")


@dataclass(frozen=True)
class QuietSpan:
    """A contiguous run of quiet segments from one source — a candidate for deletion."""

    source_id: str
    start: datetime
    end: datetime
    audio_ids: tuple[AudioSegmentId, ...]

    @property
    def duration_s(self) -> float:
        return (self.end - self.start).total_seconds()


def is_quiet(segment: SegmentVolume, threshold_db: float) -> bool:
    """Whether a segment is idle noise and nothing else — the whole test, in one place.

    Every clause is a veto, and each has been seen to matter on the real archive: an
    unmeasured or untranscribed segment is *unknown*, and a segment that produced a turn
    that still stands contains speech, however far under the threshold its mean sits.
    """
    return (
        segment.mean_db is not None
        and segment.mean_db <= threshold_db
        and segment.transcribed
        and not segment.has_speech
    )


def measure_mean_volume(path: Path) -> float | None:
    """The raw mean volume (dBFS) of an audio file via ffmpeg volumedetect, or None if
    it can't be read. Measured on the *untouched* file (no loudnorm) so the noise floor
    stays distinguishable from speech."""
    try:
        out = subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-i",
                str(path),
                "-af",
                "volumedetect",
                "-f",
                "null",
                "-",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
    except FileNotFoundError:
        return None
    match = _MEAN_VOLUME.search(out.stderr)
    return float(match.group(1)) if match else None


def _source_spans(
    segments: Sequence[SegmentVolume],
    *,
    threshold_db: float,
    min_duration_s: float,
    max_gap_s: float,
) -> list[QuietSpan]:
    """Quiet runs within a single source's segments (time-ordered)."""
    spans: list[QuietSpan] = []
    run: list[SegmentVolume] = []

    def flush() -> None:
        if run and (run[-1].end - run[0].start).total_seconds() >= min_duration_s:
            spans.append(
                QuietSpan(
                    source_id=run[0].source_id,
                    start=run[0].start,
                    end=run[-1].end,
                    audio_ids=tuple(s.audio_id for s in run),
                )
            )

    for seg in segments:
        quiet = is_quiet(seg, threshold_db)
        contiguous = not run or (seg.start - run[-1].end).total_seconds() <= max_gap_s
        if quiet and contiguous:
            run.append(seg)
            continue
        flush()
        # A quiet segment after a gap doesn't end quiet — it starts the next run.
        run = [seg] if quiet else []
    flush()
    return spans


def find_quiet_spans(
    segments: Sequence[SegmentVolume],
    *,
    threshold_db: float = QUIET_MEAN_DB,
    min_duration_s: float = MIN_QUIET_SPAN_S,
    max_gap_s: float = MAX_GAP_S,
) -> list[QuietSpan]:
    """Group each source's consecutive quiet segments (see `is_quiet`) into runs,
    keeping only spans at least `min_duration_s` long. Pure, so the grouping is
    unit-tested. Anything not provably empty — a loud segment, an unmeasured or
    untranscribed one, one bearing speech, or a hole in the recording — breaks the run
    and stays out of it."""
    by_source: dict[str, list[SegmentVolume]] = defaultdict(list)
    for seg in segments:
        by_source[seg.source_id].append(seg)

    spans: list[QuietSpan] = []
    for source_segments in by_source.values():
        spans += _source_spans(
            sorted(source_segments, key=lambda s: s.start),
            threshold_db=threshold_db,
            min_duration_s=min_duration_s,
            max_gap_s=max_gap_s,
        )
    return sorted(spans, key=lambda s: (s.start, s.source_id))


def scan_volumes(store: Store, *, batch: int = 2000) -> int:
    """Measure and cache the raw mean volume of sweepable segments not measured yet.
    ffmpeg per file is slow over the whole archive, so it's cached and resumable — this
    returns how many were measured this pass; call again while that's non-zero."""
    measured = 0
    for audio_id, path in store.audio_segments_without_volume(
        limit=batch, kinds=SWEEPABLE_KINDS
    ):
        mean_db = measure_mean_volume(Path(path))
        if mean_db is not None:
            store.set_audio_mean_volume(audio_id, mean_db)
            measured += 1
    return measured


def quiet_spans(
    store: Store,
    *,
    threshold_db: float = QUIET_MEAN_DB,
    min_duration_s: float = MIN_QUIET_SPAN_S,
) -> list[QuietSpan]:
    """The long total-quiet spans across the archive, from the cached volumes."""
    return find_quiet_spans(
        store.audio_segment_volumes(kinds=SWEEPABLE_KINDS),
        threshold_db=threshold_db,
        min_duration_s=min_duration_s,
    )
