"""Folding short add-on recordings into the session they continue."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.sessions import Recording, group_into_sessions


def _rec(key: str, start: str, minutes: float) -> Recording:
    s = datetime.fromisoformat(start).replace(tzinfo=UTC)
    return Recording(key=key, start=s, end=s + timedelta(minutes=minutes))


def test_short_addon_folds_into_the_recording_before_it() -> None:
    # The real case: a 44-min meeting, then a 0.5-min add-on ~1.5 min after it ended.
    main = _rec("main", "2026-05-20T19:01:21", 44.4)
    addon = _rec("addon", "2026-05-20T19:47:19", 0.5)
    sessions = group_into_sessions([main, addon])
    assert sessions == [[main, addon]]  # one session, anchor first
    # The add-on keeps its own (later) time — grouping never rewrites it.
    assert sessions[0][1].start > sessions[0][0].end


def test_addon_too_long_stands_alone() -> None:
    # 6 minutes isn't a fragment, even if it's right after — it's its own meeting.
    main = _rec("main", "2026-05-20T19:01:00", 44)
    later = _rec("later", "2026-05-20T19:47:00", 6)
    assert group_into_sessions([main, later]) == [[main], [later]]


def test_addon_too_far_apart_stands_alone() -> None:
    # Short, but 15 minutes after the previous piece ended — a separate sitting.
    main = _rec("main", "2026-05-20T19:01:00", 44)
    far = _rec("far", "2026-05-20T20:01:00", 0.5)  # ends 19:45, far starts 20:01
    assert group_into_sessions([main, far]) == [[main], [far]]


def test_fragment_with_no_full_anchor_stands_alone() -> None:
    # Two short pieces and no longer recording to attach to → two sessions.
    a = _rec("a", "2026-05-20T19:01:00", 2)
    b = _rec("b", "2026-05-20T19:04:00", 2)
    assert group_into_sessions([a, b]) == [[a], [b]]


def test_chained_addons_fold_into_the_anchor() -> None:
    # A long anchor plus two close add-ons all read as one session.
    main = _rec("main", "2026-05-20T19:01:00", 40)
    a1 = _rec("a1", "2026-05-20T19:43:00", 1)  # 2 min after main's 19:41 end
    a2 = _rec("a2", "2026-05-20T19:46:00", 1)  # 2 min after a1's 19:44 end
    assert group_into_sessions([main, a1, a2]) == [[main, a1, a2]]


def test_unsorted_input_is_ordered_first() -> None:
    main = _rec("main", "2026-05-20T19:01:21", 44.4)
    addon = _rec("addon", "2026-05-20T19:47:19", 0.5)
    assert group_into_sessions([addon, main]) == [[main, addon]]


def test_separate_days_never_merge() -> None:
    d1 = _rec("d1", "2026-05-20T19:01:00", 44)
    d2 = _rec("d2", "2026-05-22T14:20:00", 0.5)
    assert group_into_sessions([d1, d2]) == [[d1], [d2]]
