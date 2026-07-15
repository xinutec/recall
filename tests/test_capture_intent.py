"""Fleet-side capture intent (the Isis split): the fleet holds the desired capture state
and the Mac's last-reported actual state. Pure settings logic, so a trivial fake stands
in for the Store."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from recall import capture_control

NOW = datetime(2026, 7, 14, 12, 0, 0, tzinfo=UTC)


class FakeSettings:
    """Mimics Store.get_setting/set_setting, including its blank-reads-as-None rule."""

    def __init__(self) -> None:
        self._d: dict[str, str] = {}

    def get_setting(self, key: str) -> str | None:
        raw = self._d.get(key)
        if raw is None:
            return None
        return raw.strip() or None

    def set_setting(self, key: str, value: str) -> None:
        self._d[key] = value


def test_is_fleet_reads_the_explicit_role_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("RECALL_ROLE", raising=False)
    assert capture_control.is_fleet() is False
    monkeypatch.setenv("RECALL_ROLE", "fleet")
    assert capture_control.is_fleet() is True
    monkeypatch.setenv("RECALL_ROLE", "mac")
    assert capture_control.is_fleet() is False


def test_pause_intent_roundtrips_and_resume_clears_it() -> None:
    s = FakeSettings()
    assert capture_control.intent_until(s, NOW) is None  # running by default

    until = capture_control.intent_pause(s, NOW, minutes=30)
    assert until == NOW + timedelta(minutes=30)
    assert capture_control.intent_until(s, NOW) == until

    capture_control.intent_resume(s)
    assert capture_control.intent_until(s, NOW) is None


def test_pause_intent_is_bounded_like_the_local_pause() -> None:
    s = FakeSettings()
    until = capture_control.intent_pause(s, NOW, minutes=None)
    assert until == NOW + capture_control.MAX_PAUSE


def test_an_elapsed_intent_reads_as_running() -> None:
    # The bounded-pause safety net: a forgotten pause auto-resumes once its time passes,
    # the same guarantee the local file gets from auto_resume_if_expired.
    s = FakeSettings()
    capture_control.intent_pause(s, NOW, minutes=10)
    later = NOW + timedelta(minutes=11)
    assert capture_control.intent_until(s, later) is None


def test_reported_state_roundtrips_while_fresh() -> None:
    s = FakeSettings()
    assert capture_control.reported_state(s, NOW) is None  # nothing reported yet

    capture_control.record_reported(
        s, running=False, paused_until="2026-07-14T13:00:00+00:00", now=NOW
    )
    assert capture_control.reported_state(s, NOW) == (
        False,
        "2026-07-14T13:00:00+00:00",
    )

    capture_control.record_reported(s, running=True, paused_until=None, now=NOW)
    assert capture_control.reported_state(s, NOW) == (True, None)


def test_a_stale_report_is_ignored_so_the_fleet_falls_back_to_intent() -> None:
    # A report older than the freshness window means the Mac has stopped checking in —
    # the fleet must not keep showing its last word as if it were current.
    s = FakeSettings()
    capture_control.record_reported(s, running=True, paused_until=None, now=NOW)
    stale = NOW + capture_control._REPORT_FRESH + timedelta(seconds=1)
    assert capture_control.reported_state(s, stale) is None


def test_source_liveness_roundtrips_while_fresh() -> None:
    s = FakeSettings()
    assert capture_control.reported_source_liveness(s, NOW) is None  # nothing reported

    capture_control.record_reported(
        s,
        running=True,
        paused_until=None,
        now=NOW,
        source_liveness={"pixel9": "2026-07-14T11:59:30+00:00"},
    )
    assert capture_control.reported_source_liveness(s, NOW) == {
        "pixel9": datetime(2026, 7, 14, 11, 59, 30, tzinfo=UTC)
    }


def test_source_liveness_defaults_to_empty_when_omitted() -> None:
    # An older Mac client posts no liveness; the fleet reads an empty map (no phones
    # live), not None — None means "the Mac is not reporting at all".
    s = FakeSettings()
    capture_control.record_reported(s, running=True, paused_until=None, now=NOW)
    assert capture_control.reported_source_liveness(s, NOW) == {}


def test_stale_source_liveness_reads_as_no_report() -> None:
    # Gated by the same freshness as reported_state: a quiet Mac reports no live phones.
    s = FakeSettings()
    capture_control.record_reported(
        s, running=True, paused_until=None, now=NOW, source_liveness={"pixel9": "x"}
    )
    stale = NOW + capture_control._REPORT_FRESH + timedelta(seconds=1)
    assert capture_control.reported_source_liveness(s, stale) is None


def test_a_malformed_liveness_entry_is_dropped_not_fatal() -> None:
    s = FakeSettings()
    capture_control.record_reported(
        s,
        running=True,
        paused_until=None,
        now=NOW,
        source_liveness={"pixel9": "not-a-time", "pixel5": "2026-07-14T11:59:30+00:00"},
    )
    assert capture_control.reported_source_liveness(s, NOW) == {
        "pixel5": datetime(2026, 7, 14, 11, 59, 30, tzinfo=UTC)
    }
