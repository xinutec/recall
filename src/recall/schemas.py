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


class QuietSpanOut(TypedDict):
    """A long total-quiet span proposed for deletion (the cleanup UI). Always one
    source: several mics record the same room, and a span is one mic hearing nothing."""

    source: str
    start: str  # ISO-8601
    end: str
    durationS: float
    audioIds: list[int]
    # How empty it actually is — the evidence the list is ranked by, shown so the
    # ranking can be checked, not trusted. `silent` means nothing in the span rose
    # above this microphone's own floor: an empty room, the safest thing to delete.
    # `marginDb` is how far the loudest moment rose above that floor (negative = never).
    soundSeconds: float
    loudestDb: float | None
    marginDb: float | None
    silent: bool
    # How far the span departs from its mic's idle noise — what the list is ranked by.
    # Separates an empty room from one with somebody moving about in it, which loudness
    # cannot: a creak and a word can be equally loud.
    structure: float | None


class QuietSpansOut(TypedDict):
    items: list[QuietSpanOut]


class QuietScanOut(TypedDict):
    """The archive-measuring scan — a background job on the server, watched by the
    page (it outlives the tab, so progress is reported, not accumulated client-side).

    Two passes. `measured` is the cheap sweep (volume + waveform, ffmpeg). `analysed` is
    the speech detector listening to the candidates that turned up — slower, and the
    veto
    a deletion rests on: a span is only offered once its audio has actually been heard.
    """

    running: bool
    measured: int  # segments measured so far — durable, so this only ever goes up
    total: int
    analysed: int
    toAnalyse: int


class QuietDeletedOut(TypedDict):
    deleted: int
    freedBytes: int


class EnvelopeSegmentOut(TypedDict):
    """One capture segment inside an envelope window — the unit of play and delete."""

    audioId: int
    start: str  # ISO-8601
    end: str
    meanDb: float | None  # the cached per-minute volume; None until scanned


class SoundEventOut(TypedDict):
    """One audible thing inside the window — what the reviewer is asked to listen to."""

    start: str  # ISO-8601
    end: str
    peakDb: float


class EnvelopeOut(TypedDict):
    """A window of capture as a waveform: one peak dB per bucket, None where no audio
    exists (a gap, not silence). `points[i]` covers start + i * bucketS."""

    start: str  # ISO-8601
    end: str
    bucketS: float
    # The quiet threshold — drawn as the line a span is judged against, so what broke
    # the silence is visible rather than asserted.
    thresholdDb: float
    points: list[float | None]
    segments: list[EnvelopeSegmentOut]
    # Every sound above the threshold, so a 0.7-second bump in a 15-minute span can be
    # stepped through and heard rather than hunted for by eye.
    events: list[SoundEventOut]


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


class DaySummaryOut(TypedDict):
    """One day's generated summary (the recall layer)."""

    day: str  # yyyy-mm-dd (UTC grouping)
    text: str
    model: str  # which local LLM wrote it


class DaySummariesOut(TypedDict):
    items: list[DaySummaryOut]


class ContextOut(TypedDict):
    """The household context — background facts given to the LLM prompts."""

    text: str  # empty when unset


class TodaySummaryOut(TypedDict):
    """The running day's so-far summary (stale-while-revalidate: text may lag
    the newest turns; upToDate/pending say whether a refresh is under way)."""

    day: str  # yyyy-mm-dd (UTC)
    text: str | None  # null = nothing recorded yet / first generation in flight
    generatedAt: str | None  # when the text was generated ("as of HH:MM")
    upToDate: bool  # text reflects the day's newest turn
    pending: bool  # a background regeneration is running


# A Literal (not str) so mypy checks every returned status and the generated TS is a
# union, not `string` — same rule as Tier / AbCompareStatus above.
AskStatus = Literal["done", "pending", "error"]


class AskOut(TypedDict):
    """A grounded answer over the archive. `status`: "done" (answer ready — or null
    when retrieval found no evidence, so the UI says so instead of letting a model
    improvise), "pending" (queued for the Mac's LLM — poll GET /api/ask/{id}), or
    "error". `id` is the poll id while pending, else null. `sources` are the cited
    turns (deep links), shown even while pending. `error` carries a failure, else null.
    """

    status: AskStatus
    id: int | None
    answer: str | None
    sources: list[TranscriptOut]
    error: str | None


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


class TrainOut(TypedDict):
    items: list[TranscriptOut]
    corrections: int
    bySpeaker: dict[str, int]


class CorrectionsOut(TypedDict):
    items: list[LabelOut]
    bySpeaker: dict[str, int]


class NewIdOut(TypedDict):
    newId: int


class NewIdsOut(TypedDict):
    newIds: list[int]


class AroundOut(TypedDict):
    before: list[TranscriptOut]
    after: list[TranscriptOut]


class SuggestOut(TypedDict):
    speaker: str | None


# A/B model comparison — its lifecycle status (queued -> running -> done|error).
AbCompareStatus = Literal["queued", "running", "done", "error"]


class AbCompareScoreOut(TypedDict):
    """One human-corrected span: each model's text + WER against your ground truth,
    and the audio of that span so you can listen and judge."""

    correctionId: int
    truth: str
    textA: str
    textB: str
    werA: float
    werB: float
    audioUrl: str


class AbCompareSegmentDiffOut(TypedDict):
    """One whole-segment transcription as each model produced it (the wide text diff
    — where a model truncates or drifts over a long span shows here)."""

    audioId: int
    start: str
    changed: bool
    textA: str
    textB: str


class AbCompareRunSummaryOut(TypedDict):
    """One A/B run as the list renders it — params, status, and (once done) the
    headline mean-WER numbers; no per-span detail."""

    id: int
    source: str
    modelA: str
    modelB: str
    baseModel: str
    status: AbCompareStatus
    created: str
    meanWerA: float | None
    meanWerB: float | None
    nCorrections: int | None
    nSegments: int | None
    nChanged: int | None
    error: str | None


class AbCompareRunsOut(TypedDict):
    items: list[AbCompareRunSummaryOut]


class AbCompareRunOut(TypedDict):
    """A run's full detail: its summary plus the per-span WER evidence and the
    whole-segment diffs (empty lists until the run finishes)."""

    summary: AbCompareRunSummaryOut
    scores: list[AbCompareScoreOut]
    segmentDiffs: list[AbCompareSegmentDiffOut]
