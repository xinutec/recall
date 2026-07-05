"""Resolve relative speaker turns to enrolled people via voice embeddings.

For each current transcript segment that has audio but no resolved speaker yet,
slice the audio, embed it, match against enrolled profiles, and (if confident)
set the speaker. The embedder is injected, so this is testable with a stub.
"""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

from recall.asr import scratch_wav, slice_clip
from recall.ids import AudioSegmentId
from recall.speakerid import Embedder, SpeakerProfile, identify
from recall.store import Store

# Auto-resolving a speaker (writes the label) needs high precision.
DEFAULT_THRESHOLD = 0.5
# A re-matched guess is only re-written when its score moves more than this — so an
# unchanged voiceprint set produces no spurious writes.
_SCORE_EPSILON = 1e-4
# The displayed speaker confidence is a softmax over the enrolled people's best
# match — "how clearly is it this person vs the others" — not the raw cosine (which
# sits ~0.3 even for correct far-field matches and reads misleadingly low). The
# temperature sharpens the household's cosine range (~0.1 to 0.45) into a usable
# spread: a clear match ~0.8, a toss-up between two voices ~0.5. Tunable.
_SOFTMAX_TEMPERATURE = 0.1


def _slice_span(
    store: Store,
    audio_segment_id: AudioSegmentId,
    start: datetime,
    end: datetime,
    clip: Path,
) -> bool:
    """Slice [start, end] of a turn's source audio into `clip`. False if no audio."""
    ref = store.audio_segment_ref(audio_segment_id)
    if ref is None:
        return False
    path, audio_start = ref
    rel_start = max(0.0, (start - audio_start).total_seconds())
    rel_end = (end - audio_start).total_seconds()
    slice_clip(Path(path), clip, rel_start, rel_end)
    return True


def _profiles(store: Store) -> list[SpeakerProfile]:
    return [
        SpeakerProfile(name=name, embeddings=tuple(tuple(e) for e in embeddings))
        for name, embeddings in store.speaker_profiles().items()
    ]


def identify_segments(
    store: Store,
    embedder: Embedder,
    *,
    work_dir: Path,
    threshold: float = DEFAULT_THRESHOLD,
) -> int:
    """Resolve unidentified segments to enrolled speakers. Returns count resolved."""
    profiles = _profiles(store)
    if not profiles:
        return 0

    work_dir.mkdir(parents=True, exist_ok=True)
    resolved = 0
    for segment in store.unidentified_segments():
        if segment.audio_segment_id is None:
            continue
        with scratch_wav(work_dir / f"id-{segment.id:06d}.wav") as clip:
            if not _slice_span(
                store, segment.audio_segment_id, segment.start, segment.end, clip
            ):
                continue
            name = identify(embedder(clip), profiles, threshold=threshold)
        if name is None:
            continue
        speaker_id = store.speaker_id_for(name)
        if speaker_id is None:  # pragma: no cover - name came from the profiles
            continue
        store.resolve_speaker(segment.id, speaker_id)
        resolved += 1
    return resolved


def backfill_voiceprints(
    store: Store,
    embedder: Embedder,
    *,
    work_dir: Path,
    now: datetime,
    limit: int = 10,
) -> int:
    """Turn current human-labelled turns into reference voiceprints (offline). Each
    labelled clip is embedded and enrolled under its speaker, so any speaker work — a
    text correction *or* a session-view assign — teaches the voices. Returns how many
    were enrolled.
    """
    work_dir.mkdir(parents=True, exist_ok=True)
    enrolled = 0
    for job in store.turns_needing_voiceprint(limit=limit):
        with scratch_wav(work_dir / f"vp-{job.segment_id:06d}.wav") as clip:
            if not _slice_span(store, job.audio_segment_id, job.start, job.end, clip):
                continue
            try:
                embedding = embedder(clip)
            except Exception:  # one bad clip must never crash the always-on worker
                continue
        store.enroll_speaker(
            job.speaker,
            embedding,
            now=now,
            source_segment_id=job.segment_id,
        )
        enrolled += 1
    return enrolled


def backfill_embeddings(
    store: Store,
    embedder: Embedder,
    *,
    work_dir: Path,
    limit: int = 16,
) -> int:
    """Embed un-embedded machine turns *once* and persist the voiceprint vector.

    Embedding (pyannote) is the slow step; persisting the vector lets the speaker
    guess be re-derived for free later (rematch_speaker_guesses). Bounded per call
    so it stays light next to live capture; one bad clip never crashes the worker.
    Returns how many were embedded.
    """
    work_dir.mkdir(parents=True, exist_ok=True)
    embedded = 0
    for segment in store.segments_missing_embedding(limit=limit):
        if segment.audio_segment_id is None:
            continue
        with scratch_wav(work_dir / f"embed-{segment.id:06d}.wav") as clip:
            if not _slice_span(
                store, segment.audio_segment_id, segment.start, segment.end, clip
            ):
                continue
            try:
                vector = embedder(clip)
            except Exception:  # one bad clip must never crash the always-on worker
                continue
        store.set_embedding(segment.id, vector)
        embedded += 1
    return embedded


def rematch_speaker_guesses(store: Store) -> int:
    """Re-derive every guessable turn's speaker guess from its stored embedding and
    the *current* voiceprints — cheaply, no re-embedding — updating only the
    guesses that changed. Returns how many changed.

    This is what keeps the cached timeline guess fresh as labelling grows the
    voiceprints (so it always agrees with the live Train suggestion). The score is
    a softmax over the people's best match — a calibrated "vs the others" likelihood
    rather than the raw cosine. All vectorised over every turn at once.
    """
    import numpy as np  # noqa: PLC0415 - heavy, only when matching

    profiles = store.speaker_profiles()
    rows = store.embeddings_with_guesses()
    if not profiles or not rows:
        return 0

    people = list(profiles)
    owner: list[int] = []  # which person each voiceprint belongs to
    voiceprints: list[list[float]] = []
    for index, name in enumerate(people):
        for vec in profiles[name]:
            owner.append(index)
            voiceprints.append(list(vec))

    v = np.asarray(voiceprints, dtype=float)
    vn = v / (np.linalg.norm(v, axis=1, keepdims=True) + 1e-12)
    e = np.asarray([row[1] for row in rows], dtype=float)
    en = e / (np.linalg.norm(e, axis=1, keepdims=True) + 1e-12)
    sims = en @ vn.T  # (turns, voiceprints) cosine similarities

    owners = np.asarray(owner)
    per_person = np.full((sims.shape[0], len(people)), -1.0)
    for index in range(len(people)):
        per_person[:, index] = sims[:, owners == index].max(axis=1)
    best = per_person.argmax(axis=1)

    logits = per_person / _SOFTMAX_TEMPERATURE
    logits -= logits.max(axis=1, keepdims=True)
    probs = np.exp(logits)
    probs /= probs.sum(axis=1, keepdims=True)
    confidence = probs[np.arange(probs.shape[0]), best]

    updates: list[tuple[int, str, float]] = []
    for i, (sid, _vec, guess, score) in enumerate(rows):
        name = people[int(best[i])]
        new_score = round(float(confidence[i]), 6)
        if name != guess or score is None or abs(new_score - score) > _SCORE_EPSILON:
            updates.append((sid, name, new_score))
    if updates:
        store.set_speaker_guesses(updates)
    return len(updates)
