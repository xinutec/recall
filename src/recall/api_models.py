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


class VocabularyIn(BaseModel):
    term: str


class SessionRenameIn(BaseModel):
    title: str


class ContextIn(BaseModel):
    text: str
