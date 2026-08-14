"""What the fleet knows about whether each mic app is still running.

The iPhone app "goes down every now and then" and nothing in recall could say so
(#837). The liveness marker answers a different question on purpose — it means
*recording*, so a quiet room reads idle — and while capture is paused, which is
most of the time, there is no signal at all. These tests pin the properties that
make a beat worth trusting: it survives a phone on an older build, it cannot grow
without bound from an unauthenticated endpoint, and a value nobody can parse costs
one device rather than the answer.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.mic_alive import (
    BEATS_KEY,
    MAX_DEVICES,
    Beat,
    read_beats,
    record_beat,
)

NOW = datetime(2026, 8, 14, 9, 0, 0, tzinfo=UTC)


class FakeSettings:
    """The key-value half of Store, which is all this uses."""

    def __init__(self) -> None:
        self.values: dict[str, str] = {}

    def get_setting(self, key: str) -> str | None:
        return self.values.get(key)

    def set_setting(self, key: str, value: str) -> None:
        self.values[key] = value


def _beat(**over: object) -> Beat:
    fields: dict[str, object] = {
        "device": "iphone11",
        "app": "ios",
        "version": "1.4.0 (37)",
        "started_at": NOW - timedelta(days=3),
        "streaming": True,
        "charging": True,
        "mic_ok": True,
        "via_lan": None,
        "at": NOW,
    }
    fields.update(over)
    return Beat(**fields)  # type: ignore[arg-type]


def test_a_beat_round_trips() -> None:
    settings = FakeSettings()
    record_beat(settings, _beat())
    assert read_beats(settings) == [_beat()]


def test_each_app_keeps_its_own_line() -> None:
    # Two room phones and a carried one. A dead iPhone must not hide behind a
    # healthy Pixel — the shape that let a dead BLE receiver run for 7 hours
    # behind three live ones.
    settings = FakeSettings()
    record_beat(settings, _beat(device="iphone11"))
    record_beat(settings, _beat(device="pixel5", app="android"))
    record_beat(settings, _beat(device="pixel9", app="android"))
    assert [b.device for b in read_beats(settings)] == ["iphone11", "pixel5", "pixel9"]


def test_a_new_beat_replaces_that_devices_previous_one() -> None:
    settings = FakeSettings()
    record_beat(settings, _beat(at=NOW - timedelta(hours=1)))
    record_beat(settings, _beat(at=NOW))
    [only] = read_beats(settings)
    assert only.at == NOW


def test_streaming_false_is_recorded_rather_than_dropped() -> None:
    """A paused household is the normal state, and the beat must still arrive.

    This is the whole point of the feature: the app reports whether or not it has
    anything to stream, because the four days recall spent paused this week are
    exactly the window in which a dead app was invisible.
    """
    settings = FakeSettings()
    record_beat(settings, _beat(streaming=False))
    [only] = read_beats(settings)
    assert only.streaming is False


def test_a_restart_is_visible_because_the_beat_carries_its_own_start() -> None:
    # "Alive now" and "has been alive all week" are different answers, and only
    # the second one distinguishes a stable app from one crash-looping between
    # beats — which is what "goes down every now and then" sounds like.
    settings = FakeSettings()
    record_beat(settings, _beat(started_at=NOW - timedelta(minutes=4)))
    [only] = read_beats(settings)
    assert only.started_at == NOW - timedelta(minutes=4)


def test_an_older_build_costs_its_own_line_and_no_more() -> None:
    settings = FakeSettings()
    record_beat(settings, _beat(device="pixel5"))
    raw = settings.values[BEATS_KEY]
    settings.values[BEATS_KEY] = raw[:-1] + ', "old-build": {"app": "ios"}}'
    assert [b.device for b in read_beats(settings)] == ["pixel5"]


def test_a_half_written_value_reads_empty_rather_than_raising() -> None:
    # Read on a health endpoint's request path: a broken value must not take the
    # whole answer with it.
    settings = FakeSettings()
    settings.values[BEATS_KEY] = "{not json"
    assert read_beats(settings) == []
    settings.values[BEATS_KEY] = "[]"
    assert read_beats(settings) == []


def test_a_naive_timestamp_is_read_as_utc() -> None:
    settings = FakeSettings()
    settings.values[BEATS_KEY] = (
        '{"pixel5": {"app": "android", "version": "1", "startedAt": null,'
        ' "streaming": true, "charging": null, "at": "2026-08-14T09:00:00"}}'
    )
    [only] = read_beats(settings)
    assert only.at == NOW


def test_the_device_list_cannot_grow_without_bound() -> None:
    """⚠ The endpoint that writes these takes no credential (see webauth).

    On 2026-08-10 one test post left a `probe-mac` row in the fleet's outbox
    setting that had to be deleted by hand with sqlite3 inside the pod. Here the
    least recently heard is evicted instead, so the worst case is a bounded value
    rather than an unbounded one and a shell in production.
    """
    settings = FakeSettings()
    for n in range(MAX_DEVICES + 5):
        record_beat(
            settings, _beat(device=f"junk-{n:03d}", at=NOW - timedelta(minutes=n))
        )
    kept = read_beats(settings)
    assert len(kept) == MAX_DEVICES
    # The ones evicted are the ones heard longest ago, never the freshest.
    assert "junk-000" in {b.device for b in kept}
    assert "junk-020" not in {b.device for b in kept}


def test_a_real_phone_is_not_evicted_by_a_flood_of_fresher_junk() -> None:
    # The cap is a bound, not a queue: a phone that beat an hour ago must survive
    # noise, or the check it feeds would report the wrong devices as missing.
    settings = FakeSettings()
    record_beat(settings, _beat(device="iphone11", at=NOW - timedelta(minutes=30)))
    for n in range(MAX_DEVICES - 1):
        record_beat(settings, _beat(device=f"junk-{n:03d}", at=NOW))
    assert "iphone11" in {b.device for b in read_beats(settings)}


def test_a_mic_that_will_not_open_survives_the_round_trip() -> None:
    # #887: an app whose audio engine fails keeps beating rather than going silent,
    # so the flag saying WHY has to reach the reader intact — a False that decayed
    # to None would read as "an app too old to say" and hide a live fault.
    settings = FakeSettings()
    record_beat(settings, _beat(mic_ok=False))
    (stored,) = read_beats(settings)
    assert stored.mic_ok is False


def test_an_app_too_old_to_say_is_unknown_not_healthy() -> None:
    # Absent must never read as a working mic: the apps that predate #887 send no
    # such field, and inventing True for them would assert something never measured.
    settings = FakeSettings()
    record_beat(settings, _beat(mic_ok=None))
    (stored,) = read_beats(settings)
    assert stored.mic_ok is None
