"""Value types for the transcript store: the frozen dataclasses returned by and
passed to ``recall.store``.

Split out of ``recall.store`` so the data *shapes* are a small, dependency-light
module, separate from the query logic. ``recall.store`` re-imports these, so
``from recall.store import TranscriptSegment`` (etc.) keeps working unchanged.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import NamedTuple

from recall.asr import Word
from recall.ids import AudioSegmentId, CorrectionId, SpeakerId, TranscriptId
from recall.sources import SourceKind


class SessionSummary(NamedTuple):
    """One uploaded session (a discrete recording, e.g. a meeting) as the sessions
    list shows it. A NamedTuple (not a dataclass) so JSON/CSV formatters can still
    iterate it positionally."""

    id: str
    name: str
    start: str  # ISO, first segment start
    end: str  # ISO, last segment end
    turn_count: int
    speakers: str | None  # CSV of confirmed names, 'unknown' for the rest


@dataclass(frozen=True)
class ClusterNaming:
    """A human's naming of one diarization voice: '(this source, this cluster) is
    called <name>'. The cluster id is the key both machines share for a voice (it
    rides along every segment push), so it's what the fleet→Mac label sync travels on —
    the fleet publishes its human namings, the Mac replays them via `name_voice`."""

    source_id: str
    cluster: str
    name: str


@dataclass(frozen=True)
class CaptureEvent:
    """An immutable capture-lifecycle fact: a pause, a resume, or a dead-window — the
    record that tells a deliberate pause-gap apart from silently lost audio."""

    id: int
    utc: datetime
    kind: str
    source_id: str | None
    detail: str | None


@dataclass(frozen=True)
class TranscriptSegment:
    """A single transcribed utterance (a derived, versioned view)."""

    id: TranscriptId
    audio_segment_id: AudioSegmentId | None
    start: datetime
    end: datetime
    text: str
    language: str | None
    language_confidence: float | None
    asr_confidence: float | None
    asr_model: str
    speaker_label: str | None
    speaker_id: SpeakerId | None
    superseded_by: TranscriptId | None
    created: datetime | None = None
    provenance: str | None = None
    hidden_reason: str | None = None
    loudness: float | None = None
    # Aggressive auto speaker attribution: best-matching enrolled name + its match
    # strength (cosine in [0, 1]). Display-only; speaker_label stays authoritative.
    speaker_guess: str | None = None
    speaker_score: float | None = None
    # The diarization cluster (relative voice, e.g. 'SPEAKER_00') this turn was
    # assigned to — lets the UI group a recording's turns by voice for naming.
    speaker_cluster: str | None = None
    # The capturing source (usb / pixel9 / …), joined from the audio segment. Only
    # populated where the query needs it (the timeline, for cross-mic folding).
    source_id: str | None = None
    # Per-word timings (relative to this turn's start, seconds), for audio-exact
    # boundary edits and tight playback. None for turns made before word timings, or
    # for non-diarized turns.
    word_timings: tuple[Word, ...] | None = None


@dataclass(frozen=True)
class VocabularyTerm:
    """One household-vocabulary entry (a name/place/term the ASR is biased toward)."""

    id: int
    term: str


@dataclass(frozen=True)
class Correction:
    """A human correction: a labelled (audio span -> correct text) training pair."""

    id: CorrectionId
    audio_segment_id: AudioSegmentId | None
    start: datetime
    end: datetime
    corrected_text: str
    language: str | None
    # The labelled voice, when known. Used to stitch only same-speaker adjacent
    # turns into one training window (recall.training); None on the overlap-query
    # path that doesn't need it.
    speaker: str | None = None


@dataclass(frozen=True)
class PendingVoiceprint:
    """A current human-labelled turn awaiting enrolment (embed its clip → voiceprint).

    `segment_id` is the turn whose `speaker_label` (set by a text correction *or* a
    session-view assign) the reference embedding is built from.
    """

    segment_id: int
    speaker: str
    audio_segment_id: AudioSegmentId
    start: datetime
    end: datetime


@dataclass(frozen=True)
class LabelledFragment:
    """A human label under review — everything needed to show, play, and fix it."""

    correction_id: CorrectionId
    audio_segment_id: AudioSegmentId | None
    start: datetime
    end: datetime
    text: str
    speaker: str | None
    language: str | None


@dataclass(frozen=True)
class SourceCoverage:
    """One source's coverage of a moment: whether its raw audio recorded the window,
    and how many current turns it transcribed there — recorded vs transcribed."""

    source_id: str
    recorded: bool
    turns: int


@dataclass(frozen=True)
class SegmentVolume:
    """A capture segment as quiet-detection sees it: whether the pipeline has examined
    it, whether it left any speech behind, and whether the room was in fact empty.

    Those last two are different questions, and conflating them has broken this twice.
    *Speech* is the speech detector's business and no statistic on the waveform can
    stand in for it. *Empty* is the waveform's business and the detector cannot see it:
    a room with music playing and nobody talking holds no speech at all, and is plainly
    not empty. A segment must clear both to be swept.

    `mean_db` is the raw mean volume (None until scanned). `transcribed` says ASR has
    had its say; `has_speech` says it found words that still stand (current, not hidden
    as a hallucination). A segment carrying speech is never quiet, whatever its volume:
    a far-field sentence can sit under the noise-floor threshold on a 60-second mean,
    and deleting its audio would take the transcript with it.
    """

    audio_id: AudioSegmentId
    source_id: str
    start: datetime
    end: datetime
    mean_db: float | None
    transcribed: bool
    has_speech: bool
    # The speech detector's verdict: seconds of speech here, None if nobody has
    # listened yet. This is the veto (recall.analyse). The transcript could not be: a
    # reprocessing pass hides the turns it replaces, and a minute of real Dutch was
    # left carrying no visible turn. None is *unknown*, and unknown is never swept.
    speech_s: float | None
    # How far its most unusual moment departs from this mic's own idle noise. Ranks the
    # spans; decides nothing (recall.spectrum).
    structure: float | None
    # How much of the segment rose above *this microphone's own* sound threshold — the
    # honest test of an empty room, and the one a 60-second mean cannot do. Measured on
    # the real archive: dead air 0.0-0.2% of a minute, a door closing in an empty house
    # 3-31%, music playing 88-100%. Nothing lives in between. None until scanned.
    loud_fraction: float | None = None


@dataclass(frozen=True)
class RefineRequest:
    """A queued on-demand refine of [start, end) of one recording."""

    id: int
    source: str
    start: datetime
    end: datetime


@dataclass(frozen=True)
class AskRequest:
    """A queued "Ask the archive" job: a self-contained grounded `prompt` (built on the
    fleet from retrieved turns) the Mac's LLM generates an answer for. `sources` are the
    fleet turn ids the retrieval cited, carried through for the answer's citations."""

    id: int
    question: str
    prompt: str
    sources: tuple[int, ...]


@dataclass(frozen=True)
class AskRequestStatus:
    """The current state of an ask job: the answer once the Mac has generated it (or an
    `error`), else pending. `sources` are the turn ids to cite. `prompt` lets the Mac's
    relay confirm an adopted row still matches the job (a reused fleet id must not relay
    a stale answer)."""

    id: int
    question: str
    prompt: str
    sources: tuple[int, ...]
    answer: str | None
    error: str | None
    done: bool


@dataclass(frozen=True)
class SweepTombstone:
    """A deliberate deletion on the system of record, by segment identity — served to
    the Mac so its master-archive copy is removed too (the quiet review confirms a
    span once; both machines converge)."""

    id: int
    source: str
    start: datetime


@dataclass(frozen=True)
class SweepEvidence:
    """What the Mac's *own* database knows about a segment a fleet tombstone wants
    deleted — the ground the Mac stands on when it decides whether to honour the sweep.

    The one-way VPN makes the Mac the protected master archive precisely so a
    compromised Isis cannot destroy the household's audio. A sweep is therefore a
    *request*, checked against this evidence: the Mac only deletes what its own VAD
    already measured as speechless capture (`speech_s == 0`, no surviving turn, a
    captured — not uploaded — kind). Anything else it keeps. So the worst a hostile
    Isis can command is the deletion of audio the Mac itself already scored as an idle
    room; deliberate removal of real speech stays a Mac-local act."""

    audio_id: AudioSegmentId
    kind: SourceKind
    speech_s: float | None  # the Mac's VAD verdict; None = never measured here
    has_speech: bool  # a current, visible turn (human or machine) stands on it


@dataclass(frozen=True)
class UploadJob:
    """An uploaded session segment awaiting the Mac's ASR (the fleet has no ML).

    Derived, never enqueued: any UPLOAD-kind segment the Mac has not processed yet
    (`transcribed_utc` unset) IS the job — so an upload can't be lost to a missed
    enqueue, and the queue survives a DB restore."""

    audio_id: int
    source: str
    title: str  # the source's display name, so the Mac registers it verbatim
    file: str  # blob filename under the fleet archive (fetched via /sync/audio/file)
    start: datetime
    end: datetime
    sample_rate: int
    channels: int


@dataclass(frozen=True)
class LiveSummary:
    """The running day's provisional summary, stamped with the day-state
    watermark it saw — fresh iff that still matches Store.day_watermark."""

    day: str
    text: str
    model: str
    watermark: str
    generated_utc: str


@dataclass(frozen=True)
class AbCompareJob:
    """One A/B model-comparison run: its parameters, status, and (once done) result.

    `start`/`end` are None for the whole recording. The `mean_wer_*`/`n_*` summary
    and `result_json` are populated only when `status == "done"`; `list_…` returns
    rows without the (large) `result_json`, `get_…` includes it.
    """

    id: int
    source: str
    start: datetime | None
    end: datetime | None
    model_a: str
    model_b: str
    base_model: str
    status: str
    created: datetime
    started: datetime | None
    done: datetime | None
    error: str | None
    result_json: str | None
    mean_wer_a: float | None
    mean_wer_b: float | None
    n_corrections: int | None
    n_segments: int | None
    n_changed: int | None
