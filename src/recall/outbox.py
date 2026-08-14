"""What each phone still holds that it has been told to send.

A recording the user approved and the phone could not deliver is the one piece of
this system's state that lived nowhere the fleet could see it. The meeting
recorder 401ed from the day the feature was written; WorkManager retried politely
out to a backoff of +1h23m, the audio stayed safe in the outbox, and the screen
said "N recordings waiting to upload" — a sentence that reads the same whether
the host is unreachable, the token is wrong, or nobody has pressed Upload yet.
The offline-first design that made the failure non-destructive is exactly what
made it silent (#77, found by #628).

So the phone says what it is holding, and the fleet grades it. Two properties
make that honest:

* **Every pass reports, including the ones that find nothing.** Only reporting
  failures would leave the last bad reading standing after the queue drained, and
  a check that cannot go back to green is a check that gets muted.
* **Silence is not health.** A phone that cannot reach Isis cannot report either,
  so a report going stale is itself a finding — graded by the collector that
  reads this, in the same way fleetwatch already treats a producer that stops.

Stored in `settings` rather than a table of its own, following the Mac's mirror
report (`capture_control.report_state`): this is last-known status, it is
rewritten whole every few minutes, and none of it is worth a migration or a row
of history.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Final, Protocol

REPORTS_KEY: Final = "device_outbox_reports"

# Both client-supplied and both bound for the same reason: they land in one JSON
# value in the settings table and are rendered into a health check. A device id is
# `<model>-<8 hex>` by construction (Prefs.deviceId) and a reason is one of a
# handful of sentences (UploadFailure), so these caps are far above anything real.
MAX_DEVICE_LEN: Final = 64
MAX_REASON_LEN: Final = 200


class Settings(Protocol):
    """The key-value half of `Store` — all this needs, and what makes it testable."""

    def get_setting(self, key: str) -> str | None: ...

    def set_setting(self, key: str, value: str) -> None: ...


@dataclass(frozen=True)
class OutboxReport:
    """One phone's outbox, as it last described it.

    `oldest_queued_at` is when the oldest undelivered recording was made, not when
    it was approved: the recording's start is the only time the phone carries, and
    the question being asked — how long has this been stuck — wants the older of
    the two anyway.
    """

    device: str
    queued: int
    oldest_queued_at: datetime | None
    failing: int
    reason: str | None
    at: datetime


def record_report(settings: Settings, report: OutboxReport) -> None:
    """Store this phone's report, replacing whatever it said before."""
    existing = _raw(settings)
    existing[report.device[:MAX_DEVICE_LEN]] = {
        "queued": report.queued,
        "oldestQueuedAt": (
            None
            if report.oldest_queued_at is None
            else report.oldest_queued_at.isoformat()
        ),
        "failing": report.failing,
        "reason": None if report.reason is None else report.reason[:MAX_REASON_LEN],
        "at": report.at.isoformat(),
    }
    settings.set_setting(REPORTS_KEY, json.dumps(existing))


def read_reports(settings: Settings) -> list[OutboxReport]:
    """Every phone's last report, oldest device id first.

    ⚠ Never raises. This is read on a health endpoint's request path, and a phone
    on an older build or a half-written value must cost that one device's line
    rather than the whole answer — the same rule the Mac's mirror report follows.
    """
    out: list[OutboxReport] = []
    for device, raw in sorted(_raw(settings).items()):
        report = _one(device, raw)
        if report is not None:
            out.append(report)
    return out


def _raw(settings: Settings) -> dict[str, object]:
    stored = settings.get_setting(REPORTS_KEY)
    if not stored:
        return {}
    try:
        loaded = json.loads(stored)
    except ValueError:
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _one(device: str, raw: object) -> OutboxReport | None:
    if not isinstance(raw, dict):
        return None
    try:
        return OutboxReport(
            device=device[:MAX_DEVICE_LEN],
            queued=int(raw["queued"]),
            oldest_queued_at=_when(raw.get("oldestQueuedAt")),
            failing=int(raw["failing"]),
            reason=_text(raw.get("reason")),
            at=_required(raw["at"]),
        )
    except (LookupError, TypeError, ValueError):
        return None


def _when(value: object) -> datetime | None:
    """⚠ A naive timestamp means UTC, and `.astimezone(UTC)` would NOT say so — it
    reads a naive value as *local* time. The phone formats with `ISO_INSTANT` today,
    so every value carries a `Z`; a build that stopped would have each queue read an
    hour younger here in summer, moving a stuck upload back under the threshold that
    exists to notice it."""
    if value is None:
        return None
    parsed = datetime.fromisoformat(str(value))
    return parsed.astimezone(UTC) if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _required(value: object) -> datetime:
    when = _when(value)
    if when is None:
        msg = "a report must carry when it was received"
        raise ValueError(msg)  # caught by _one, costing this device its line
    return when


def _text(value: object) -> str | None:
    if value is None:
        return None
    text = str(value)[:MAX_REASON_LEN]
    return text or None
