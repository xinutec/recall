"""Request body models for the recall API (pydantic).

The ROUTES these bodies belong to are `recalld`'s (`recalld/src/`) — this file is
the generator input for the frontend's TypeScript, not a server. `scripts/gen_models.py`
reads it alongside ``recall.schemas`` (the response shapes) and writes
``frontend/src/app/models.ts``, so a shape changed here and not in Rust shows up as a
type error in the app rather than a silent wire mismatch.
"""

from __future__ import annotations

from pydantic import BaseModel


class ClientLog(BaseModel):
    level: str = "error"
    message: str
    stack: str | None = None
    url: str | None = None


class TelemetryEvent(BaseModel):
    """One thing that happened in the client.

    ``kind`` is ``nav`` for a route change, where ``label`` is absent, or ``tap``
    for a control, where ``label`` is its visible text, verbatim. ``at`` is the
    client's clock in epoch milliseconds — a batch arrives all at once, so the
    server's receive time cannot order the events inside it and the client's can.
    """

    # ⚠ No defaults: `Telemetry.enqueue` sends all four fields on every event
    # (`{kind, path, label, at: Date.now()}`), with `label` null for a nav rather
    # than absent. Defaults here described a client that does not exist, and the
    # generated TypeScript inherited that fiction — dev-lint's mirror check
    # caught the drift against the Rust port on 2026-09-07.
    kind: str
    path: str
    label: str | None
    at: int


class OutboxIn(BaseModel):
    """What a phone still holds that it was told to send.

    Posted after every upload pass, including the ones that find nothing — a
    report only sent on failure would leave the last bad reading standing after
    the queue drained, and a check that cannot go back to green gets muted.

    `reason` is the phone's own text for the last failure. It is composed on the
    phone from a fixed set of sentences plus an HTTP status, never from the
    exception's message, so it cannot carry the bearer token.
    """

    device: str
    queued: int = 0
    oldestQueuedAt: str | None = None
    failing: int = 0
    reason: str | None = None


class HeartbeatIn(BaseModel):
    """A mic app saying it is still running (#837).

    Sent hourly whether or not the app has anything to stream. That is the point:
    recall's liveness marker means *recording* — it is refreshed only by audio above
    the silence floor, so a quiet room and a dead app read alike — and while capture
    is paused there is no stream at all. The beat is the only signal that survives
    both.

    Every field except `device` is optional so that an app on an older build still
    counts as alive. A beat that arrives says the thing that matters; the rest is
    detail for the reader once it stops arriving.
    """

    device: str
    app: str = ""  # "ios" | "android" — which recorder, for the check's wording
    version: str = ""  # app build, so a restart into a new build is legible
    startedAt: str | None = None  # when THIS process started, ISO-8601
    streaming: bool = False  # does it currently have the recorder?
    charging: bool | None = None  # room phones are mains-powered; discharging leads
    # False when the app is up but its audio engine would not open (#887). Optional:
    # an app too old to say sends nothing, which must not read as a working mic.
    micOk: bool | None = None
    # Set by the Mac's LAN relay (#888), never by a phone — a beat that had to come
    # the back way means the tunnel is down, which is worth seeing rather than
    # papering over. The relay strips any value a phone sends.
    viaLan: bool | None = None


class CorrectIn(BaseModel):
    id: int
    text: str
    speaker: str | None = None
    start: str | None = None  # ISO; overrides the audio span (boundary editor)
    end: str | None = None
    language: str | None = None  # fix a mis-detected language (nl heard as en)


class VoiceNameIn(BaseModel):
    cluster: str  # the diarization voice to name
    name: str | None = None  # null/blank clears the name


class TurnSpeakerIn(BaseModel):
    name: str | None = None  # reassign one turn to a voice; null/blank clears it


class AssignSpanIn(BaseModel):
    startTurn: int
    startChar: int
    endTurn: int
    endChar: int
    name: str


class RefineRequestIn(BaseModel):
    source: str
    start: str  # ISO 8601
    end: str  # ISO 8601


class ReassignIn(BaseModel):
    speaker: str


class VocabularyIn(BaseModel):
    term: str


class SessionRenameIn(BaseModel):
    title: str
