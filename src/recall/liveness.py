"""Per-source liveness: which recorders are measurably recording right now.

Every source reduces to a last-proved-recording time, read from its liveness
marker (recall.capture.ALIVE_FILE) — refreshed by the ingest pump while a phone
streams real signal, and by the capture watchdog while the local mic's closed
segments decode to real audio. "Active" therefore means *recording*, never just
connected: a phone streaming digital silence, or a mic in a startup dead-window,
reads idle (docs/capture-loss-plan.md). The freshness window matches how each
marker is refreshed — per chunk for a stream, per watchdog poll for the mic.
Pure — no I/O — so it's unit-tested.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime, timedelta

from recall.sources import SourceKind

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


def active_window(kind: str, *, on_fleet: bool) -> timedelta:
    """How fresh `kind`'s marker must be to call the source recording."""
    streamed = kind == SourceKind.TCP_PCM.value
    base = ACTIVE_WITHIN if streamed else WATCHDOG_ACTIVE_WITHIN
    return base + (_FLEET_REPORT_LAG if on_fleet else timedelta(0))


@dataclass(frozen=True)
class SourceStatus:
    source_id: str
    name: str
    kind: str
    last_active: datetime | None
    active: bool


def source_statuses(
    sources: list[tuple[str, str, str]],
    last_active: Mapping[str, datetime | None],
    now: datetime,
    *,
    on_fleet: bool = False,
) -> list[SourceStatus]:
    """Combine registered sources (id, name, kind) with their last-activity time."""
    statuses: list[SourceStatus] = []
    for source_id, name, kind in sources:
        seen = last_active.get(source_id)
        window = active_window(kind, on_fleet=on_fleet)
        active = seen is not None and now - seen < window
        statuses.append(SourceStatus(source_id, name, kind, seen, active))
    return statuses
