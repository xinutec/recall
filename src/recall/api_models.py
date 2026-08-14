"""Request body models for the recall API (pydantic).

Split out of ``recall.api`` so the request *shapes* are separate from the route
logic — mirroring ``recall.schemas`` (the response shapes) and ``store_models``.
"""

from __future__ import annotations

from pydantic import BaseModel, Field


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

    kind: str
    path: str
    label: str | None = None
    at: int = 0


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


class UnintelligibleIn(BaseModel):
    id: int


class UnhideIn(BaseModel):
    id: int


class NudgeIn(BaseModel):
    edge: str  # "start" | "end"
    delta: float  # seconds, signed (negative = earlier, positive = later)


class RefineRequestIn(BaseModel):
    source: str
    start: str  # ISO 8601
    end: str  # ISO 8601


class AbCompareStartIn(BaseModel):
    """Start an A/B comparison. Models default to the deployed pairing — the live
    stock model vs the fine-tuned adapter on its base — so the common case is one
    click; advanced callers can override any of the three. `from`/`to` (ISO) restrict
    it to a window; omit both for the whole recording."""

    source: str
    frm: str | None = Field(default=None, alias="from")
    to: str | None = None
    modelA: str | None = None
    modelB: str | None = None
    baseModel: str | None = None


class ReassignIn(BaseModel):
    speaker: str


class FragmentIn(BaseModel):
    start: str
    end: str
    text: str
    speaker: str | None = None


class SplitIn(BaseModel):
    id: int
    fragments: list[FragmentIn]


class AskIn(BaseModel):
    question: str


class QuietDeleteIn(BaseModel):
    audioIds: list[int]  # the capture segments of a confirmed quiet span, to delete


class VocabularyIn(BaseModel):
    term: str


class SessionRenameIn(BaseModel):
    title: str


class ContextIn(BaseModel):
    text: str
