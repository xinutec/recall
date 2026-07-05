"""Semantic identifier types — make the int-typed keys un-confusable.

A transcript's own ``id``, its ``audio_segment_id``, and a ``speaker_id`` are all
plain ints, so nothing stops you passing one where another is expected — most
sharply when two are adjacent arguments, as in
``resolve_speaker(transcript_id, speaker_id)``. Wrapping each in a distinct
``NewType`` makes that a mypy error.

Zero runtime cost and not brittle: a ``SpeakerId`` *is* an ``int`` everywhere it's
used as one (arithmetic, SQL parameters, dict keys), so nothing downstream breaks.
Only *constructing* one from a raw int — at the database boundary — needs an
explicit wrap, which is exactly where a mix-up could otherwise slip in.
"""

from __future__ import annotations

from typing import NewType

# A row id in `transcript_segments` (and what `superseded_by` points at).
TranscriptId = NewType("TranscriptId", int)
# A row id in `audio_segments` — the retained raw-audio index.
AudioSegmentId = NewType("AudioSegmentId", int)
# A row id in `speakers` — an enrolled person (the strict, resolved attribution).
SpeakerId = NewType("SpeakerId", int)
# A row id in `corrections` — a human-labelled training pair.
CorrectionId = NewType("CorrectionId", int)
