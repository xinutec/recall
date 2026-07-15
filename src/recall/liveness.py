"""Per-source liveness: which recorders are streaming right now.

A *local* capturer (the USB mic) is known to the host directly — its agent is
loaded and capture isn't paused. A *remote* recorder (a phone) is known indirectly:
the ingest server refreshes a marker file while the phone's socket is connected
(recall.stream_server), so the host that owns the socket reports the liveness — the
phone sends no heartbeat. Either way it reduces to a last-active time per source;
this turns those into active/idle against a freshness window. Pure — no I/O — so
it's unit-tested.
"""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime, timedelta

# The marker is refreshed ~every second while a phone streams; a source is idle once
# its last signal is older than this. Small, because the signals are real-time (the
# host-owned marker / agent state), not buffered audio.
ACTIVE_WITHIN = timedelta(seconds=5)

# On the fleet (Isis) the liveness signal arrives via the Mac's ~5s mirror report, not a
# local marker, so a streaming phone's last-active reads up to a report-cadence older
# here than on the Mac. Widen the window to absorb that lag (report cadence + marker
# staleness + margin), while still flipping to idle within a couple of missed reports.
FLEET_ACTIVE_WITHIN = timedelta(seconds=12)


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
    active_within: timedelta = ACTIVE_WITHIN,
) -> list[SourceStatus]:
    """Combine registered sources (id, name, kind) with their last-activity time."""
    statuses: list[SourceStatus] = []
    for source_id, name, kind in sources:
        seen = last_active.get(source_id)
        active = seen is not None and now - seen < active_within
        statuses.append(SourceStatus(source_id, name, kind, seen, active))
    return statuses
