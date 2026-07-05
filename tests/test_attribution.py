"""Per-word speaker-attribution scoring against human ground truth."""

from __future__ import annotations

from recall.attribution import TruthSpan, attribution_report, score_attribution


def test_perfect_attribution_scores_one() -> None:
    truth = [TruthSpan(0.0, 1.0, "Pippijn"), TruthSpan(1.0, 2.0, "Dr Lee")]
    # cluster A is always in Pippijn's span, B in the doctor's
    predicted = [(0.2, "A"), (0.6, "A"), (1.2, "B"), (1.7, "B")]
    score = score_attribution(predicted, truth)
    assert (score.words, score.correct) == (4, 4)
    assert score.accuracy == 1.0


def test_cluster_ids_are_mapped_to_names_by_majority() -> None:
    # Cluster ids needn't match names: "X" is mostly Pippijn, "Y" mostly the doctor.
    truth = [TruthSpan(0.0, 1.0, "Pippijn"), TruthSpan(1.0, 2.0, "Dr Lee")]
    predicted = [(0.2, "X"), (0.6, "X"), (1.2, "Y"), (1.7, "Y")]
    assert score_attribution(predicted, truth).accuracy == 1.0


def test_a_crossed_boundary_word_counts_wrong() -> None:
    # One Pippijn word leaked into cluster B (the doctor's) — 3/4 correct.
    truth = [TruthSpan(0.0, 1.0, "Pippijn"), TruthSpan(1.0, 2.0, "Dr Lee")]
    predicted = [(0.2, "A"), (0.9, "B"), (1.2, "B"), (1.7, "B")]
    score = score_attribution(predicted, truth)
    assert (score.words, score.correct) == (4, 3)
    assert score.accuracy == 0.75


def test_under_clustering_one_voice_for_two_people_is_penalised() -> None:
    # A single cluster spans both speakers: it maps to the majority (the doctor, 3),
    # so the lone Pippijn word it swallowed is wrong → 3/4.
    truth = [TruthSpan(0.0, 1.0, "Pippijn"), TruthSpan(1.0, 2.0, "Dr Lee")]
    predicted = [(0.5, "A"), (1.2, "A"), (1.5, "A"), (1.8, "A")]
    assert score_attribution(predicted, truth).correct == 3


def test_words_outside_every_truth_span_are_skipped() -> None:
    truth = [TruthSpan(0.0, 1.0, "Pippijn")]
    predicted = [(0.5, "A"), (5.0, "A")]  # the second word has no ground truth
    score = score_attribution(predicted, truth)
    assert score.words == 1  # only the in-span word is scored


def test_no_overlap_is_zero_words_not_a_crash() -> None:
    score = score_attribution([(9.0, "A")], [TruthSpan(0.0, 1.0, "Pippijn")])
    assert (score.words, score.correct, score.accuracy) == (0, 0, 0.0)


def test_report_localises_a_boundary_crossing_error() -> None:
    truth = [TruthSpan(0.0, 1.0, "Pippijn"), TruthSpan(1.0, 5.0, "Dr Lee")]
    predicted = [
        (0.5, "A"),
        (0.95, "A"),  # Pippijn → A
        (1.2, "A"),  # the doctor's word leaked into A, right by the 1.0s change
        (2.0, "B"),
        (3.0, "B"),  # Dr → B
    ]
    r = attribution_report(predicted, truth, boundary_window=1.0, short_span=2.0)
    assert (r.words, r.correct) == (5, 4)
    assert r.errors_by_speaker == {"Dr Lee": 1}  # whose word was taken
    # the single error sits next to the speaker change, not in the interior
    assert r.interior_accuracy == 1.0
    assert r.near_accuracy < 1.0


def test_report_flags_a_lost_short_interjection() -> None:
    # a 0.4s interjection swallowed by one big cluster
    truth = [
        TruthSpan(0.0, 3.0, "Dr Lee"),
        TruthSpan(3.0, 3.4, "Pippijn"),
        TruthSpan(3.4, 6.0, "Dr Lee"),
    ]
    predicted = [(1.0, "A"), (2.0, "A"), (3.2, "A"), (4.0, "A"), (5.0, "A")]
    r = attribution_report(predicted, truth, short_span=1.0)
    assert r.errors_by_speaker == {"Pippijn": 1}  # the interjection
    assert (r.short_words, r.short_correct) == (1, 0)  # it's in a short turn, and wrong
