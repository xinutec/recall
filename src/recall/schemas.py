"""Typed response shapes for the JSON API.

Each endpoint (and the helpers that build its rows) returns a ``TypedDict`` from
here instead of a bare ``dict[str, object]``, so mypy checks the wire shape — the
keys and value types the Angular front-end depends on — at build time. No runtime
cost: these are plain dicts at runtime, the names exist purely for the type checker
(and FastAPI's inferred schema). Reach for Pydantic only if we later want to
auto-generate the front-end models from these.
"""

from __future__ import annotations

from typing import Literal, TypedDict

# The analysis tier that produced a turn — the exact vocabulary `_tier()` emits.
# A Literal (not str) so mypy checks both ends and the generated TS carries the
# real union instead of a loose `string`.
Tier = Literal["live", "transcribed", "diarized", "corrected"]


class TranscriptOut(TypedDict):
    """One turn as the timeline/search/review lists render it."""

    id: int
    start: str
    end: str
    text: str
    language: str | None
    speaker: str | None
    speakerConfirmed: bool
    speakerConfidence: float | None
    confidence: float | None
    loudness: float | None
    model: str
    tier: Tier
    hidden: str | None
    audioUrl: str
    source: str | None
    # The diarization cluster (relative voice) — for grouping a session's turns by
    # voice so a person can be named once per voice. Null until refined.
    cluster: str | None


class SourceStatusOut(TypedDict):
    """One recorder's liveness for the fleet view: is it streaming, and when last."""

    id: str
    name: str
    kind: str
    active: bool
    lastActive: str | None
    #: Is it RUNNING — bytes arriving, whatever is on them? Distinct from
    #: `active`, which is the consent signal ("your voice is being captured
    #: audibly") and therefore goes out in a silent room. Answering the second
    #: question with the first is how geb read "off" while recording (#1428).
    recording: bool
    lastDelivered: str | None


class PromptOut(TypedDict):
    """The household glossary as Whisper's `initial_prompt`, or None when the
    vocabulary is empty — the runner then sends no biasing rather than an empty
    string, which Whisper would treat as a prompt of nothing."""

    prompt: str | None


class SourcesOut(TypedDict):
    items: list[SourceStatusOut]


class OutboxOut(TypedDict):
    """One phone's undelivered recordings, as it last reported them.

    `reason` is the phone's own wording for the last failure (recall's Android
    `UploadFailure`), which is composed from constants and a status code and so
    never carries the token. Ages are left to the reader: this says *when*, and
    the fleetwatch collector that grades it decides what is too long.
    """

    device: str
    queued: int
    oldestQueuedAt: str | None
    failing: int
    reason: str | None
    at: str


class OutboxesOut(TypedDict):
    items: list[OutboxOut]


class HeartbeatOut(TypedDict):
    """One mic app's last "I am still here", as it last said so.

    `streaming` and `charging` are the app's own view, carried but never graded —
    every honest app reports `streaming: false` while the household is paused, and a
    carried phone is off charge all day. They are here so that when the beats DO
    stop, the last one says what state the app was in when it went.

    `micOk` is the exception in kind: False means the app is running but its audio
    engine would not open, which is a fault rather than a mode. Still not graded
    here — this type reports, the collector decides.

    Ages are left to the reader, as with `OutboxOut`: this says *when*, and the
    fleetwatch collector that grades it decides what is too long.
    """

    device: str
    app: str
    version: str
    startedAt: str | None
    streaming: bool
    charging: bool | None
    micOk: bool | None
    viaLan: bool | None
    at: str


class HeartbeatsOut(TypedDict):
    items: list[HeartbeatOut]


class SessionOut(TypedDict):
    """One uploaded session — a discrete recording (e.g. a meeting) — for the list."""

    id: str
    title: str
    start: str
    end: str
    turnCount: int
    speakers: list[str]


class SessionsOut(TypedDict):
    items: list[SessionOut]


class TranscriptBubbleOut(TypedDict):
    """One coalesced speaker bubble in an exported transcript."""

    start: str  # ISO 8601 with the local offset (e.g. 2026-01-15T10:41:51+01:00)
    speaker: str  # a confirmed name, or 'SPEAKER_nn'/'unknown' if not yet confirmed
    text: str


class TranscriptExportOut(TypedDict):
    """A session's clean, finalised transcript — for rendering into a doc/website.

    Consecutive same-speaker turns are merged into one bubble; only the current,
    human-corrected state is included (no superseded/hidden turns, no alternates).
    Deterministic, so re-fetching unchanged data yields byte-identical output.
    """

    session: str
    date: str | None  # ISO 8601 (local) of the first bubble; null if empty
    speakers: list[str]  # confirmed names present, in first-seen order
    turns: list[TranscriptBubbleOut]


class MomentOut(TypedDict):
    """One wall-clock moment: the best mic's turn(s) (`primary`, its speaker split
    kept) plus the other mics' overlapping versions (`alternates`, for compare)."""

    start: str
    end: str
    primary: list[TranscriptOut]
    alternates: list[TranscriptOut]
    sources: list[str]


class ConversationOut(TypedDict):
    """A gap-segmented run of turns, folded into per-moment cards."""

    start: str
    end: str
    turnCount: int
    speakers: list[str]
    preview: str
    moments: list[MomentOut]


class LabelOut(TypedDict):
    """A labelled correction fragment under review."""

    id: int
    text: str
    speaker: str | None
    language: str | None
    start: str
    audioUrl: str


class CaptureOut(TypedDict):
    """Capture state as spec-vs-status, so a client can render "Pausing…" instead
    of flapping between the intent it just set and the mic's not-yet-caught-up
    report. running/pausedUntil stay the confirmed view (the mic's word when it is
    reporting, else the desired state) for older clients."""

    running: bool
    pausedUntil: str | None
    desiredRunning: bool
    desiredPausedUntil: str | None
    # The mic has confirmed the desired state; False = transitioning (or unreachable).
    settled: bool
    # The mic is checking in (always True on the capturing host itself).
    micReachable: bool
    # Fingerprint of this state. A client passes it back as GET /api/capture?known=
    # with ?wait= to long-poll: the request hangs until the state differs or the
    # wait elapses, so a change reaches clients in ~RTT instead of a poll interval.
    stateToken: str


class OkOut(TypedDict):
    ok: bool


class SoundEventOut(TypedDict):
    """One audible thing inside the window — what the reviewer is asked to listen to."""

    start: str  # ISO-8601
    end: str
    peakDb: float


class SpeakerNamesOut(TypedDict):
    names: list[str]  # known speaker names, for autocompleting voice naming


class AssignResultOut(TypedDict):
    touched: int  # turns touched by a span assign (reassign / split / merge)


class VoiceSuggestionsOut(TypedDict):
    suggestions: dict[str, str]  # {cluster: suggested name} from voiceprints


class VocabularyTermOut(TypedDict):
    id: int
    term: str


class VocabularyOut(TypedDict):
    items: list[VocabularyTermOut]


AskStatus = Literal["done", "pending", "error"]


class ItemsOut(TypedDict):
    """A bare list of turns (search / review / transcripts / hidden)."""

    items: list[TranscriptOut]


class PageOut(TypedDict):
    """A page of turns with a has-more cursor flag (timeline)."""

    items: list[TranscriptOut]
    hasMore: bool


class ConversationsOut(TypedDict):
    items: list[ConversationOut]
    hasMore: bool


class CorrectionsOut(TypedDict):
    items: list[LabelOut]
    bySpeaker: dict[str, int]


class NewIdOut(TypedDict):
    newId: int


class NewIdsOut(TypedDict):
    newIds: list[int]


class SuggestOut(TypedDict):
    speaker: str | None


# A/B model comparison — its lifecycle status (queued -> running -> done|error).
AbCompareStatus = Literal["queued", "running", "done", "error"]
