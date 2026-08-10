"""What the fleet knows about recordings still sitting on a phone.

The meeting recorder 401ed from the day it was written and nobody knew (#628,
fixed in d844053). WorkManager retried politely to a backoff of +1h23m, the audio
stayed safe in the outbox, and `MeetingActivity` said "N recordings waiting to
upload" — which is indistinguishable from "not approved yet" and from "not home
yet". The offline-first design that made the failure non-destructive is exactly
what made it silent (#77).

`#78` gave the phone the words; this gives the words somewhere to go. An approved
recording that cannot be delivered is phone-side state no fleet component could
see, and the doctor on the Mac — five minutes away from every check in the
fleet — would never have known.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.outbox import OutboxReport, read_reports, record_report

NOW = datetime(2026, 8, 10, 20, 0, 0, tzinfo=UTC)


class FakeSettings:
    """The key-value half of Store, which is all this uses."""

    def __init__(self) -> None:
        self.values: dict[str, str] = {}

    def get_setting(self, key: str) -> str | None:
        return self.values.get(key)

    def set_setting(self, key: str, value: str) -> None:
        self.values[key] = value


def _report(**over: object) -> OutboxReport:
    fields: dict[str, object] = {
        "device": "pixel9",
        "queued": 2,
        "oldest_queued_at": NOW - timedelta(hours=3),
        "failing": 1,
        "reason": "Not authorised — check the upload token in Settings.",
        "at": NOW,
    }
    fields.update(over)
    return OutboxReport(**fields)  # type: ignore[arg-type]


def test_a_report_round_trips() -> None:
    settings = FakeSettings()
    record_report(settings, _report())
    assert read_reports(settings) == [_report()]


def test_each_phone_keeps_its_own_line() -> None:
    # Two phones record meetings. One stuck queue must not hide behind another
    # phone's healthy one, and the newer report must not overwrite the other.
    settings = FakeSettings()
    record_report(settings, _report(device="pixel9"))
    record_report(settings, _report(device="pixel5", queued=0, failing=0, reason=None))
    assert {r.device for r in read_reports(settings)} == {"pixel5", "pixel9"}


def test_a_later_report_replaces_the_earlier_one_for_that_phone() -> None:
    settings = FakeSettings()
    record_report(settings, _report(queued=5))
    record_report(settings, _report(queued=0, failing=0, reason=None, at=NOW))
    assert [r.queued for r in read_reports(settings)] == [0]


def test_an_empty_queue_is_a_report_worth_making() -> None:
    """⚠ The clearing signal, and the reason the phone reports after every pass.

    If only failures were sent, the last non-zero reading would stand forever and
    the check would stay red after the queue drained — a monitor that cannot go
    back to green is one that gets muted, and a muted check is where this whole
    task started.
    """
    settings = FakeSettings()
    record_report(settings, _report(queued=3, failing=3))
    record_report(
        settings, _report(queued=0, oldest_queued_at=None, failing=0, reason=None)
    )
    [only] = read_reports(settings)
    assert (only.queued, only.failing, only.oldest_queued_at) == (0, 0, None)


def test_nothing_reported_yet_is_an_empty_list_not_a_crash() -> None:
    assert read_reports(FakeSettings()) == []


def test_a_corrupt_record_is_dropped_rather_than_raising() -> None:
    """Best-effort status, not control — the same rule as the Mac's mirror report.

    This is read on the request path of a health endpoint. A phone on an older
    build, or a half-written value, must cost one device's line rather than the
    whole answer.
    """
    settings = FakeSettings()
    record_report(settings, _report(device="pixel9"))
    settings.values["device_outbox_reports"] = (
        '{"pixel9": {"queued": "banana"}, "pixel5": {"queued": 1, "failing": 0, '
        '"oldestQueuedAt": null, "reason": null, "at": "2026-08-10T20:00:00+00:00"}}'
    )
    assert [r.device for r in read_reports(settings)] == ["pixel5"]


def test_unparseable_json_reads_as_nothing_reported() -> None:
    settings = FakeSettings()
    settings.values["device_outbox_reports"] = "{not json"
    assert read_reports(settings) == []


def test_a_device_id_cannot_grow_without_bound() -> None:
    # The id is client-supplied and lands in a JSON blob in the settings table.
    settings = FakeSettings()
    record_report(settings, _report(device="x" * 500))
    [only] = read_reports(settings)
    assert len(only.device) <= 64


def test_the_reason_shown_to_the_fleet_is_bounded_too() -> None:
    settings = FakeSettings()
    record_report(settings, _report(reason="y" * 2000))
    [only] = read_reports(settings)
    assert only.reason is not None
    assert len(only.reason) <= 200
