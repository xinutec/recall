"""Value types for the transcript store: the frozen dataclasses returned by and
passed to ``recall.store``.

Split out of ``recall.store`` so the data *shapes* are a small, dependency-light
module, separate from the query logic. ``recall.store`` re-imports these, so
``from recall.store import TranscriptSegment`` (etc.) keeps working unchanged.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime

from recall.asr import Word
from recall.ids import AudioSegmentId, CorrectionId, SpeakerId, TranscriptId


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
class RefineRequest:
    """A queued on-demand refine of [start, end) of one recording."""

    id: int
    source: str
    start: datetime
    end: datetime


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
