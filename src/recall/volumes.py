"""Measure every captured segment once: its mean volume and its envelope.

One decode per file, ever. The archive is ~12k minute-long files, so nothing here
may be recomputed on demand; the cached measurement is what calibration, the
waveform drawing and the doctor all read.

This was extracted from `recall.quiet` when the quiet-review surface was cut
(architecture.md, "Scope of the rebuilt product"). The scanner survived the
feature it was written for, because calibration and the worker depend on it — and
it is named for what it does rather than for what used to consume it.

⚠ **The lesson the quiet review paid for, kept because it still governs how this
measurement may be USED.** Two questions look alike and are not:

* **"Was anyone speaking?"** Only a speech detector answers that; a minute's mean
  volume cannot, and was once made to try against a -60 dB line. The archive
  disproves that line on every microphone — the quietest minute the detector
  actually heard speech in averages -68.7 dB on the USB mic, -83.2 on the pixel9,
  -85.1 on the pixel5. A minute holding four seconds of far-field Dutch is
  fifty-six seconds of silence, and the mean is the silence. A statistic speech
  does not move cannot be made into a speech detector by any threshold.
* **"Was the room empty?"** Only the waveform answers *that*, and a speech
  detector cannot: music playing to an empty sofa contains no speech whatever.

So volume is evidence about EMPTINESS, never about speech, and the per-microphone
floor is what makes it mean anything (`recall.calibrate`). Stage D4's silero
detector answers the first question; this answers the second.

⚠ A file that will not decode is recorded as UNDECODABLE rather than skipped.
Skipping would leave it pending for ever, re-decoded by every scan, and the
archive would never read as fully measured. Its volume stays NULL, so it reads as
unknown — never as silence.
"""

from __future__ import annotations

from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from recall.envelope import UNDECODABLE, encode_envelope, measure
from recall.sources import SWEEPABLE_KINDS
from recall.store import Store

# Decodes run several at a time. Measured: the work is ~99.5% ffmpeg (104 ms a
# file, against 0.5 ms of arithmetic), and ffmpeg is a subprocess, so the GIL is
# free while it runs — eight at once measures 2.6x the files a minute of one at a
# time. Writes stay on the calling thread, since the store is one connection.
SCAN_WORKERS = 8


def scan_segments(
    store: Store,
    *,
    batch: int = 2000,
    workers: int = SCAN_WORKERS,
    should_stop: Callable[[], bool] | None = None,
) -> int:
    """Decode the segments not examined yet, caching each one's mean volume and
    envelope. Returns how many were examined this pass; call again while that is
    non-zero.

    `should_stop` is checked between chunks so a long scan can be cancelled
    promptly.
    """
    pending = store.audio_segments_unmeasured(limit=batch, kinds=SWEEPABLE_KINDS)
    examined = 0
    with ThreadPoolExecutor(max_workers=workers) as pool:
        for i in range(0, len(pending), workers):
            if should_stop is not None and should_stop():
                break
            chunk = pending[i : i + workers]
            results = pool.map(lambda item: measure(Path(item[1])), chunk)
            for (audio_id, _path), result in zip(chunk, results, strict=True):
                if result is None:
                    store.set_audio_measurement(audio_id, None, UNDECODABLE)
                else:
                    store.set_audio_measurement(
                        audio_id, result.mean_db, encode_envelope(result.buckets)
                    )
                examined += 1
    return examined
