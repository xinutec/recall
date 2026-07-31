"""Diarization hyperparameters — the clustering knobs, and leaving them alone.

pyannote ships `clustering.threshold` 0.7046 and `min_cluster_size` 12, tuned on meeting
corpora. On household far-field audio the pipeline both over-splits one person into
several clusters and merges two people across a handover, and those two failures do not
cost the same: extra clusters of one person still map to that person by majority, while
a merged cluster takes the head of the next speaker's sentence. So biasing toward
over-splitting is worth measuring — which needs the knob to be reachable at all.

Which knob, measured rather than assumed: `min_cluster_size` counts 10 s windows, so a
short second speaker in a 60 s segment cannot reach 12 and is absorbed into the dominant
cluster; dropping it to 3 took one segment from 6 clusters to 8. The threshold does the
opposite of what its name suggests — the shipped value sits near a cluster-count
maximum, and 0.40 and 0.90 both collapse that segment to 2.
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
    # The default path must be byte-identical to not calling instantiate at all —
    # production diarization does not move because a knob became reachable.
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
    # pyannote hands back its live parameter dict; editing it in place would change the
    # pipeline for every later call in the process, including the daemon's.
    before = SHIPPED["clustering"]["threshold"]
    tuned_parameters(SHIPPED, threshold=0.4, min_cluster_size=1)
    assert SHIPPED["clustering"]["threshold"] == before
    assert SHIPPED["clustering"]["min_cluster_size"] == 12


def test_a_pipeline_without_clustering_parameters_is_refused() -> None:
    # Better a loud failure than silently scoring a sweep that never applied.
    with pytest.raises(ValueError, match="clustering"):
        tuned_parameters({"segmentation": {}}, threshold=0.5, min_cluster_size=None)
