"""The LAN fallback for the mic heartbeat: what the Mac accepts, and what it
refuses to pass on."""

from __future__ import annotations

import json

import pytest

from recall.beat_relay import RelayRejected, relayed


def _raw(**over: object) -> bytes:
    body: dict[str, object] = {
        "device": "iphone11",
        "app": "ios",
        "version": "1.2 (3)",
        "startedAt": "2026-08-11T07:00:00Z",
        "streaming": False,
        "charging": True,
        "micOk": True,
    }
    body.update(over)
    return json.dumps(body).encode()


def test_a_relayed_beat_keeps_what_the_phone_said() -> None:
    out = relayed(_raw())
    assert out["device"] == "iphone11"
    assert out["app"] == "ios"
    assert out["version"] == "1.2 (3)"
    assert out["startedAt"] == "2026-08-11T07:00:00Z"
    assert out["streaming"] is False
    assert out["charging"] is True
    assert out["micOk"] is True


def test_a_relayed_beat_says_it_came_the_back_way() -> None:
    # The whole point of marking it: a phone reaching the fleet only over the LAN is
    # alive AND has a broken tunnel, and those are different facts. Without this the
    # second one is invisible forever.
    assert relayed(_raw())["viaLan"] is True


def test_a_phone_cannot_claim_it_came_over_the_vpn() -> None:
    # `viaLan` is the relay's own testimony about how the beat arrived, so a value
    # from the wire must be overwritten rather than trusted — otherwise the one
    # field that reports a broken tunnel is the one field a broken client can deny.
    assert relayed(_raw(viaLan=False))["viaLan"] is True


def test_unknown_keys_are_dropped_rather_than_forwarded() -> None:
    # This endpoint is unauthenticated by design (a mic app has never held a
    # credential), and it now sits on the LAN rather than behind the VPN. An
    # allowlist keeps it from being a hole through which arbitrary fields reach the
    # fleet store.
    out = relayed(_raw(somethingElse="x", at="2000-01-01T00:00:00Z"))
    assert "somethingElse" not in out
    # `at` in particular: the server stamps it from its own clock on purpose, so a
    # relayed beat must not be able to backdate or postdate itself.
    assert "at" not in out


def test_a_beat_must_name_a_device() -> None:
    with pytest.raises(RelayRejected):
        relayed(json.dumps({"app": "ios"}).encode())
    with pytest.raises(RelayRejected):
        relayed(json.dumps({"device": ""}).encode())


def test_junk_is_refused_not_forwarded() -> None:
    with pytest.raises(RelayRejected):
        relayed(b"not json")
    with pytest.raises(RelayRejected):
        relayed(b"[1,2,3]")  # a list is not a beat


def test_an_overlong_device_is_refused() -> None:
    # The fleet store keys on this and evicts by it; an unbounded string from an
    # unauthenticated LAN caller is not something to pass along.
    with pytest.raises(RelayRejected):
        relayed(_raw(device="x" * 500))
