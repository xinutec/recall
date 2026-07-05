"""Speaker-matching logic: cosine similarity and identification (pure)."""

from __future__ import annotations

import math

import pytest

from recall.speakerid import (
    SpeakerProfile,
    cosine_similarity,
    identify,
)


def test_cosine_identical_is_one() -> None:
    assert math.isclose(cosine_similarity([1.0, 2.0, 3.0], [1.0, 2.0, 3.0]), 1.0)


def test_cosine_orthogonal_is_zero() -> None:
    assert cosine_similarity([1.0, 0.0], [0.0, 1.0]) == 0.0


def test_cosine_scale_invariant() -> None:
    assert math.isclose(cosine_similarity([1.0, 1.0], [3.0, 3.0]), 1.0)


def test_cosine_zero_vector() -> None:
    assert cosine_similarity([0.0, 0.0], [1.0, 1.0]) == 0.0


def test_cosine_dimension_mismatch_raises() -> None:
    with pytest.raises(ValueError, match="dimension mismatch"):
        cosine_similarity([1.0, 2.0], [1.0])


def test_profile_uses_best_matching_voiceprint() -> None:
    profile = SpeakerProfile(name="ann", embeddings=((1.0, 0.0), (0.0, 1.0)))
    # closer to the second enrolled voiceprint
    expected = cosine_similarity([0.1, 1.0], [0.0, 1.0])
    assert math.isclose(profile.similarity([0.1, 1.0]), expected)


def test_identify_picks_best_above_threshold() -> None:
    profiles = [
        SpeakerProfile(name="ann", embeddings=((1.0, 0.0, 0.0),)),
        SpeakerProfile(name="bob", embeddings=((0.0, 1.0, 0.0),)),
    ]
    assert identify([0.9, 0.1, 0.0], profiles, threshold=0.8) == "ann"
    assert identify([0.1, 0.9, 0.0], profiles, threshold=0.8) == "bob"


def test_identify_returns_none_when_below_threshold() -> None:
    profiles = [SpeakerProfile(name="ann", embeddings=((1.0, 0.0, 0.0),))]
    # a voice orthogonal to the only profile -> unknown
    assert identify([0.0, 0.0, 1.0], profiles, threshold=0.5) is None


def test_identify_no_profiles_is_unknown() -> None:
    assert identify([1.0, 0.0], [], threshold=0.5) is None
