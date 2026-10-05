"""The clustering overrides (see `recall.diarize.pyannote_diarize`), and
leaving the shipped values alone when none is given.

Over-splitting one person costs less than merging two: extra clusters of one
person still map to them by majority, while a merged cluster takes the start
of the next speaker's sentence.
"""

from __future__ import annotations

from typing import Any

import pytest

from recall.diarize import tuned_parameters

SHIPPED: dict[str, Any] = {
    "segmentation": {"min_duration_off": 0.0},
    "clustering": {
        "method": "centroid",
        "min_cluster_size": 12,
        "threshold": 0.7045654963945799,
    },
}


def test_no_overrides_returns_the_shipped_parameters_unchanged() -> None:
    assert tuned_parameters(SHIPPED, threshold=None, min_cluster_size=None) == SHIPPED


def test_threshold_override_leaves_every_other_parameter_alone() -> None:
    tuned = tuned_parameters(SHIPPED, threshold=0.5, min_cluster_size=None)
    assert tuned["clustering"]["threshold"] == 0.5
    assert tuned["clustering"]["min_cluster_size"] == 12
    assert tuned["clustering"]["method"] == "centroid"
    assert tuned["segmentation"] == {"min_duration_off": 0.0}


def test_min_cluster_size_override_is_independent() -> None:
    tuned = tuned_parameters(SHIPPED, threshold=None, min_cluster_size=3)
    assert tuned["clustering"]["min_cluster_size"] == 3
    assert tuned["clustering"]["threshold"] == SHIPPED["clustering"]["threshold"]


def test_the_source_parameters_are_not_mutated() -> None:
    # pyannote hands back its live parameter dict.
    before = SHIPPED["clustering"]["threshold"]
    tuned_parameters(SHIPPED, threshold=0.4, min_cluster_size=1)
    assert SHIPPED["clustering"]["threshold"] == before
    assert SHIPPED["clustering"]["min_cluster_size"] == 12


def test_a_pipeline_without_clustering_parameters_is_refused() -> None:
    # Rather than score a sweep that never applied.
    with pytest.raises(ValueError, match="clustering"):
        tuned_parameters({"segmentation": {}}, threshold=0.5, min_cluster_size=None)
