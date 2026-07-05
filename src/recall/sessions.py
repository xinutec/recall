"""Group discrete recordings into sessions.

Some recorders (a phone voice-memo app) split one sitting into several files — a long
recording plus short add-ons made moments later, when you stop and restart. This folds
those short fragments back into the session they belong to, so a meeting that was
stopped and restarted reads as one meeting rather than several.

Grouping only decides *membership*: the real start/end of every piece is preserved
untouched, so the timeline still shows each fragment at its true time, with the true
gap before it. We never glue a fragment onto the end of the recording it joins.

Pure (no I/O), so it's unit-tested.
"""

from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime, timedelta

# A piece shorter than this, starting within this long after the previous piece, folds
# into the (longer) recording before it instead of standing as its own session.
FRAGMENT_MAX = timedelta(minutes=5)
SESSION_GAP_MAX = timedelta(minutes=10)


@dataclass(frozen=True)
class Recording:
    key: str  # caller's handle (a filename / id); opaque here
    start: datetime
    end: datetime

    @property
    def duration(self) -> timedelta:
        return self.end - self.start


def group_into_sessions(
    recordings: Sequence[Recording],
    *,
    fragment_max: timedelta = FRAGMENT_MAX,
    session_gap_max: timedelta = SESSION_GAP_MAX,
) -> list[list[Recording]]:
    """Group recordings into sessions, ordered by start time. Each returned session is
    a list whose first element is its anchor (the longer recording).

    A recording opens a new session, except a short fragment (duration < fragment_max)
    that begins within session_gap_max of the previous piece's end, in a session whose
    anchor is itself a full recording (>= fragment_max) — that fragment folds in. Gaps
    are measured end-to-start (the silence between recordings); an overlap counts as
    zero gap. Times are never altered.
    """
    sessions: list[list[Recording]] = []
    for rec in sorted(recordings, key=lambda r: r.start):
        if sessions:
            session = sessions[-1]
            anchor_is_full = session[0].duration >= fragment_max
            gap = rec.start - session[-1].end
            if anchor_is_full and rec.duration < fragment_max and gap < session_gap_max:
                session.append(rec)
                continue
        sessions.append([rec])
    return sessions
