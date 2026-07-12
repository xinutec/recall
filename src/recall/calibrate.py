"""What counts as a *sound* on a given microphone — measured from that microphone.

A sound threshold is a property of a mic, not of the system. Measured across this
archive's own envelopes, the four recorders do not resemble each other at all:

    mic        floor median   floor p99.9   faintest real speech
    usb            -69 dB        -51 dB          -50 dB
    pixel5         -89 dB        -62 dB          -70 dB
    pixel9         -91 dB        -52 dB          -68 dB
    iphone11       -81 dB        -56 dB          -57 dB

One constant cannot serve those. A threshold tuned on the USB mic (-52 dB) sits *above*
every word a phone has ever recorded: applied to a phone it would report "no sound at
all in this span" over audio containing full sentences — the exact false reassurance the
review exists to prevent. So each source gets its own, from two facts about itself:

* **the ceiling of its noise floor** — the 99.9th percentile of its idle buckets, above
  which a bucket is no longer the mic breathing;
* **the faintest real speech it has ever recorded** — the quietest peak among segments
  that produced a turn that still stands.

The threshold is the *lower* of (floor ceiling) and (faintest speech, less a margin).
Where a mic hears clearly, the floor decides and the list stays short. Where it hears
badly — the phones, whose faint speech sits below their own floor's crests — speech
decides, and the list gets longer. That asymmetry is deliberate: a span that lists too
many sounds costs clicks, and one that lists too few costs words.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from recall.envelope import DEFAULT_EVENT_DB, decode_envelope
from recall.quiet import QUIET_MEAN_DB
from recall.store import Store

# The floor's crest ceiling. Above its 99.9th percentile a bucket is not idle mic noise.
FLOOR_PERCENTILE = 99.9
# Headroom under the faintest speech a mic has recorded — for the fainter word it has
# not recorded yet. 2 dB is small, but the bound it guards is a hard one.
SPEECH_MARGIN_DB = 2.0
# Too few segments and the percentiles are noise. Below this a source stays uncalibrated
# and falls back to the default, rather than committing to a number it cannot support.
MIN_QUIET_SEGMENTS = 20


@dataclass(frozen=True)
class Calibration:
    """One microphone's measured thresholds, and what they were measured from."""

    source_id: str
    floor_ceiling_db: float
    faintest_speech_db: float | None
    threshold_db: float
    quiet_segments: int
    speech_segments: int

    @property
    def bounded_by_speech(self) -> bool:
        """Whether it was the mic's own faint speech, not its floor, that set the bar —
        true of a recorder whose words sit down among its own noise (the phones)."""
        return (
            self.faintest_speech_db is not None
            and self.threshold_db < self.floor_ceiling_db
        )


def _peaks(envelopes: list[bytes]) -> list[float]:
    return [max(b) for b in (decode_envelope(e) for e in envelopes) if b]


def calibrate_source(
    source_id: str, quiet: list[bytes], speech: list[bytes]
) -> Calibration | None:
    """Measure one microphone. None if it hasn't been heard enough to say.

    Pure, so the rule is testable without a store: `quiet` are the envelopes of its idle
    segments, `speech` those of segments that produced a turn that still stands.
    """
    if len(quiet) < MIN_QUIET_SEGMENTS:
        return None
    buckets = np.concatenate([np.asarray(decode_envelope(e)) for e in quiet if e])
    if not buckets.size:
        return None

    floor_ceiling = float(np.percentile(buckets, FLOOR_PERCENTILE))
    speech_peaks = _peaks(speech)
    faintest_speech = min(speech_peaks) if speech_peaks else None

    threshold = floor_ceiling
    if faintest_speech is not None:
        # The lower of the two, always: never place the bar above a word this mic has
        # actually heard, however tidy that would make the list.
        threshold = min(threshold, faintest_speech - SPEECH_MARGIN_DB)

    return Calibration(
        source_id=source_id,
        floor_ceiling_db=floor_ceiling,
        faintest_speech_db=faintest_speech,
        threshold_db=threshold,
        quiet_segments=len(quiet),
        speech_segments=len(speech),
    )


def calibrate(store: Store) -> list[Calibration]:
    """Re-measure every source from its stored envelopes and persist the thresholds.

    Cheap — it reads the shapes the scan already decoded — so it runs at the end of
    every scan: a mic's floor drifts (a new room, a new gain), and a new mic is unknown.
    """
    done: list[Calibration] = []
    for source_id in store.sweepable_source_ids():
        result = calibrate_source(
            source_id,
            store.quiet_envelopes(source_id, quiet_below_db=QUIET_MEAN_DB),
            store.speech_envelopes(source_id),
        )
        if result is None:
            continue
        store.set_source_event_db(source_id, result.threshold_db)
        done.append(result)
    return done


def event_threshold(store: Store, source_id: str) -> float:
    """The level at which a bucket is a sound on this microphone. Falls back to the
    default only for a source not yet heard enough to measure — by the time one has
    enough audio to offer a span, the scan has calibrated it."""
    measured = store.source_event_db(source_id)
    return DEFAULT_EVENT_DB if measured is None else measured
