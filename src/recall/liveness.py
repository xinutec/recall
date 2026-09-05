"""Per-source liveness: which recorders are measurably recording right now.

Every source reduces to a last-proved-recording time, read from its liveness
marker (recall.capture.ALIVE_FILE) — refreshed by the ingest pump while a phone
streams real signal, and by the capture watchdog while the local mic's closed
segments decode to real audio. "Active" therefore means *recording*, never just
connected: a phone streaming digital silence, or a mic in a startup dead-window,
reads idle. The freshness window matches how each
marker is refreshed — per chunk for a stream, per watchdog poll for the mic.
Pure — no I/O — so it's unit-tested.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import NamedTuple

from recall.sources import SourceKind, SourceRow

# A streamed phone's marker is refreshed sub-second while signal flows; idle once
# it's older than this.
ACTIVE_WITHIN = timedelta(seconds=5)
# The mic's marker is refreshed by the dead-segment watchdog each healthy poll
# (recall.runner, every 30 s): two missable polls plus margin.
WATCHDOG_ACTIVE_WITHIN = timedelta(seconds=75)
# On the fleet (Isis) the markers arrive via the Mac's ~5 s mirror report, not a
# local file, so a source's last-proved time reads up to a report-cadence older
# there. Widen every window by that lag plus margin.
_FLEET_REPORT_LAG = timedelta(seconds=7)
# A STORE-AND-FORWARD recorder streams to nothing, so no marker of its is ever
# refreshed; it proves itself by delivering a closed segment instead. That
# evidence arrives once per segment, not per chunk: 60 s of audio must CLOSE,
# then wait up to the 60 s upload timer, then transfer and verify. Five minutes
# covers that with margin — wide enough not to flap, narrow enough that a dead
# recorder does not read live for long.
DELIVERED_ACTIVE_WITHIN = timedelta(minutes=5)


def active_window(kind: SourceKind, *, on_fleet: bool) -> timedelta:
    """How fresh `kind`'s marker must be to call the source recording."""
    base = ACTIVE_WITHIN if kind is SourceKind.TCP_PCM else WATCHDOG_ACTIVE_WITHIN
    return base + (_FLEET_REPORT_LAG if on_fleet else timedelta(0))


class Evidence(NamedTuple):
    """What a recorder's DELIVERIES prove, kept as two times because they answer
    two different questions and collapsing them is the #1428 bug.

    `delivered` — newest segment sent, whatever was on it: "is it running".
    `speech` — newest segment that could be someone talking: "is my voice being
    captured audibly". None when nothing audible has been measured.
    """

    delivered: datetime | None
    speech: datetime | None


@dataclass(frozen=True)
class SourceStatus:
    source_id: str
    name: str
    kind: SourceKind
    last_active: datetime | None
    active: bool
    #: Is this recorder RUNNING — bytes arriving, whatever is on them? Separate
    #: from `active` on purpose. `active` is the consent signal ("your voice is
    #: being captured audibly"), so it goes out in a silent room; `recording` is
    #: the operational one, and answering the second with the first is how geb
    #: came to read "off" while recording perfectly (#1428).
    recording: bool = False
    last_delivered: datetime | None = None


def source_statuses(
    sources: list[SourceRow],
    last_active: Mapping[str, datetime | None],
    now: datetime,
    *,
    on_fleet: bool = False,
    delivered: Mapping[str, Evidence] | None = None,
) -> list[SourceStatus]:
    """Combine registered sources with their last-activity time.

    `delivered` carries each source's `Evidence` — the second way a recorder can
    prove itself, and the only way for one that streams to nothing. Both times
    must be CAPTURE times, never arrival times: a backlog draining hours late
    arrives now but proves nothing about now.
    """
    delivered = delivered or {}
    statuses: list[SourceStatus] = []
    for row in sources:
        seen = last_active.get(row.id)
        window = active_window(row.kind, on_fleet=on_fleet)
        marker_fresh = seen is not None and now - seen < window
        # ⚠ A marker that went stale RECENTLY is a deliberate stop, and that is
        # NEWER information than a segment captured just before it. Without this,
        # delivery-proof resurrects a phone the moment its owner stops it:
        # measured 2026-09-05, pixel9 stayed green for the full five minutes
        # after stopping, where it used to go idle in twelve seconds. A phone
        # streams as its PRIMARY path, so its marker falling silent IS the
        # event; geb's marker is hours stale only because it never streams at
        # all. How stale is what tells the two apart.
        stopped_recently = (
            seen is not None
            and not marker_fresh
            and now - seen < DELIVERED_ACTIVE_WITHIN
        )

        def _fresh(when: datetime | None, *, stopped: bool = stopped_recently) -> bool:
            return (
                not stopped
                and when is not None
                and now - when < DELIVERED_ACTIVE_WITHIN
            )

        evidence = delivered.get(row.id)
        shipped = evidence.delivered if evidence else None
        heard = evidence.speech if evidence else None
        recording = marker_fresh or _fresh(shipped)
        # The marker is itself signal-gated (refreshed only above the silence
        # floor), so a fresh marker IS evidence of audible speech.
        active = marker_fresh or _fresh(heard)
        proved = [t for t in (seen, heard) if t is not None]
        delivered_at = [t for t in (seen, shipped) if t is not None]
        statuses.append(
            SourceStatus(
                row.id,
                row.name,
                row.kind,
                max(proved, default=None),
                active,
                recording,
                max(delivered_at, default=None),
            )
        )
    return statuses
