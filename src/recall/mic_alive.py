"""Whether each mic app is still running — the one thing nothing else could answer.

recall has always had a per-source liveness marker, and it deliberately does not
mean this. `stream_server` refreshes it only for a chunk whose peak clears
SILENCE_PEAK, so "active" means *recording*: a phone streaming digital silence
reads idle on purpose, because nobody should speak trusting a dot the audio
cannot back. The cost of that correctness is that a quiet room and a dead app are
the same reading.

Two more things make the gap total rather than partial:

* A phone that vanishes without a FIN leaves the ingest socket half-open, so no
  `ingest_disconnect` is written either and the connection looks open forever
  (#838).
* While capture is paused the listener is closed and every stream dropped, so
  there is no signal of any kind. Capture is normally paused for days at a time,
  which is exactly when an app dying goes unnoticed until the next resume.

So the app says so itself, once an hour, whether or not it is streaming. The beat
arriving proves the process is alive; `streaming` says whether it can reach the
recorder; `started_at` says whether it is the same process as last hour, which is
what tells "up all week" from "crashing and relaunching".

Sent to the CONTROL plane (Isis over WireGuard), not the recorder on the LAN. The
apps already split the two, and the control plane is reachable from anywhere — so
a carried phone beats from away from home too, and "out of the house" stops
looking like "dead" without needing a room/carried flag per device.

Stored in `settings` for the same reason as `outbox`: last-known status, rewritten
whole every hour, worth no migration and no row of history. Any history worth
keeping is the graded check's, which fleetwatch already retains.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Final, Protocol

BEATS_KEY: Final = "device_mic_heartbeats"

# How often an app is asked to beat. Everything downstream is expressed in
# multiples of this, so changing the cadence does not silently leave a threshold
# describing the old one.
BEAT_EVERY_MINUTES: Final = 60

# All client-supplied, all bound: they land in one JSON value in the settings
# table and are rendered into a health check. A device id is `<model>-<8 hex>` by
# construction and the rest are short tokens, so these caps sit far above anything
# real.
MAX_DEVICE_LEN: Final = 64
MAX_TEXT_LEN: Final = 64

# ⚠ The endpoint that writes these is unauthenticated by design (see webauth), so
# the number of devices is client-controlled. On 2026-08-10 a single test post put
# a `probe-mac` row into the fleet's outbox setting that had to be removed by hand
# with sqlite3 inside the pod. A cap turns that from surgery into eviction: the
# least recently heard drops out. Three phones exist; sixteen is room to be wrong
# without the value growing without bound.
MAX_DEVICES: Final = 16


class Settings(Protocol):
    """The key-value half of `Store` — all this needs, and what makes it testable."""

    def get_setting(self, key: str) -> str | None: ...

    def set_setting(self, key: str, value: str) -> None: ...


@dataclass(frozen=True)
class Beat:
    """One mic app saying it is still there.

    `streaming` is the app's own view of whether it currently has the recorder:
    deliberately NOT a verdict input, because while capture is paused every honest
    app reports False, and a check that goes yellow every time the household pauses
    is a check that gets muted. It is carried so that when the beat DOES stop, the
    last reading says whether it was recording when it went.

    `charging` is here for the room phones, which are mains-powered: an iPhone or a
    Pixel discharging in a room is the leading indicator of the death this exists to
    catch. Not graded, for the same reason — a carried phone is off charge all day.

    `mic_ok` is False when the app is running but its audio engine would not open
    (permission revoked, the mic held by another app). It exists because #887 made
    such an app keep beating: before that a failed mic silenced the beat entirely,
    and the check went red for the wrong reason — accidental, but it WAS a signal,
    and fixing the silence would have removed it. None from an app too old to say.
    """

    device: str
    app: str
    version: str
    started_at: datetime | None
    streaming: bool
    charging: bool | None
    mic_ok: bool | None
    via_lan: bool | None
    at: datetime


def record_beat(settings: Settings, beat: Beat) -> None:
    """Store this app's beat, replacing whatever it said before."""
    existing = _raw(settings)
    existing[beat.device[:MAX_DEVICE_LEN]] = {
        "app": beat.app[:MAX_TEXT_LEN],
        "version": beat.version[:MAX_TEXT_LEN],
        "startedAt": None if beat.started_at is None else beat.started_at.isoformat(),
        "streaming": bool(beat.streaming),
        "charging": None if beat.charging is None else bool(beat.charging),
        "micOk": None if beat.mic_ok is None else bool(beat.mic_ok),
        "viaLan": None if beat.via_lan is None else bool(beat.via_lan),
        "at": beat.at.isoformat(),
    }
    settings.set_setting(BEATS_KEY, json.dumps(_evicted(existing)))


def read_beats(settings: Settings) -> list[Beat]:
    """Every app's last beat, oldest device id first.

    ⚠ Never raises. This is read on a health endpoint's request path, and a phone on
    an older build or a half-written value must cost that one device's line rather
    than the whole answer — the rule the mirror report and the outbox both follow.
    """
    out: list[Beat] = []
    for device, raw in sorted(_raw(settings).items()):
        beat = _one(device, raw)
        if beat is not None:
            out.append(beat)
    return out


def _evicted(entries: dict[str, object]) -> dict[str, object]:
    """Keep the MAX_DEVICES most recently heard. Ordered by the stored `at`, so an
    entry that cannot be read at all sorts oldest and is the first to go."""
    if len(entries) <= MAX_DEVICES:
        return entries
    ranked = sorted(entries.items(), key=lambda kv: _sort_key(kv[1]), reverse=True)
    return dict(ranked[:MAX_DEVICES])


def _sort_key(raw: object) -> str:
    if not isinstance(raw, dict):
        return ""
    at = raw.get("at")
    return str(at) if at is not None else ""


def _raw(settings: Settings) -> dict[str, object]:
    stored = settings.get_setting(BEATS_KEY)
    if not stored:
        return {}
    try:
        loaded = json.loads(stored)
    except ValueError:
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _one(device: str, raw: object) -> Beat | None:
    if not isinstance(raw, dict):
        return None
    try:
        return Beat(
            device=device[:MAX_DEVICE_LEN],
            app=_text(raw.get("app")),
            version=_text(raw.get("version")),
            started_at=_when(raw.get("startedAt")),
            streaming=bool(raw["streaming"]),
            charging=_flag(raw.get("charging")),
            mic_ok=_flag(raw.get("micOk")),
            via_lan=_flag(raw.get("viaLan")),
            at=_required(raw["at"]),
        )
    except (LookupError, TypeError, ValueError):
        return None


def _when(value: object) -> datetime | None:
    """⚠ A naive timestamp means UTC, and `.astimezone(UTC)` would NOT say so — it
    reads a naive value as *local* time, so a phone on a build that sends no offset
    would have every beat shifted by the Mac's offset (an hour, in summer) and read
    as older than it is. Everything here is UTC by convention; say it explicitly."""
    if value is None:
        return None
    parsed = datetime.fromisoformat(str(value))
    return parsed.astimezone(UTC) if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _required(value: object) -> datetime:
    when = _when(value)
    if when is None:
        msg = "a beat must carry when it was received"
        raise ValueError(msg)  # caught by _one, costing this device its line
    return when


def _text(value: object) -> str:
    return "" if value is None else str(value)[:MAX_TEXT_LEN]


def _flag(value: object) -> bool | None:
    return None if value is None else bool(value)
