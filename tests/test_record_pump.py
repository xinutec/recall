"""record()'s metered pump: the mic's liveness marker is refreshed from the PCM
actually flowing into the segmenter — the same measured-signal rule as the phone
ingest pump — so after a resume the dot turns on within seconds of real sound
instead of waiting ~75s for the first closed segment. Digital silence still reads
idle, and the archive path stays gap-free through the pump."""

from __future__ import annotations

import os
import threading
from pathlib import Path

from recall.capture import CaptureConfig, StreamMeter
from recall.runner import _pump_metered, record
from recall.sources import AudioSource, SourceKind


class _FakeProducer:
    def __init__(self) -> None:
        self.terminated = False

    def terminate(self) -> None:
        self.terminated = True


def _pcm(sample: int, count: int) -> bytes:
    return int(sample).to_bytes(2, "little", signed=True) * count


def test_pump_streams_bytes_and_marks_alive_on_signal(tmp_path: Path) -> None:
    # Everything read from the producer lands in the segmenter (archive first),
    # audible chunks refresh the marker, and producer EOF closes the segmenter's
    # stdin so it finalises cleanly.
    prod_r, prod_w = os.pipe()
    cons_r, cons_w = os.pipe()
    producer_out = os.fdopen(prod_r, "rb", buffering=0)
    consumer_in = os.fdopen(cons_w, "wb")
    payload = _pcm(2000, 8192)
    os.write(prod_w, payload)
    os.close(prod_w)  # producer EOF
    received = bytearray()

    def drain() -> None:
        with os.fdopen(cons_r, "rb") as f:
            received.extend(f.read())

    reader = threading.Thread(target=drain)
    reader.start()
    producer = _FakeProducer()
    _pump_metered(
        producer_out,
        consumer_in,
        producer,  # type: ignore[arg-type]
        tmp_path,
        StreamMeter(16000, 1),
    )
    reader.join(timeout=5.0)
    assert bytes(received) == payload
    assert consumer_in.closed  # EOF passed on — the segmenter finalises
    assert (tmp_path / ".alive").exists()
    assert not producer.terminated


def test_pump_digital_silence_never_marks_alive(tmp_path: Path) -> None:
    prod_r, prod_w = os.pipe()
    cons_r, cons_w = os.pipe()
    producer_out = os.fdopen(prod_r, "rb", buffering=0)
    consumer_in = os.fdopen(cons_w, "wb")
    os.write(prod_w, _pcm(1, 8192))  # the pixel9 dead path: amplitude-1 "silence"
    os.close(prod_w)
    drained = threading.Thread(target=lambda: os.fdopen(cons_r, "rb").read())
    drained.start()
    _pump_metered(
        producer_out,
        consumer_in,
        _FakeProducer(),  # type: ignore[arg-type]
        tmp_path,
        StreamMeter(16000, 1),
    )
    drained.join(timeout=5.0)
    assert not (tmp_path / ".alive").exists()


def test_pump_terminates_producer_when_the_segmenter_dies(tmp_path: Path) -> None:
    # A dead segmenter must not leave sox blocked on a full pipe: the pump
    # terminates the producer so record() returns and the respawn recovers.
    prod_r, prod_w = os.pipe()
    cons_r, cons_w = os.pipe()
    producer_out = os.fdopen(prod_r, "rb", buffering=0)
    consumer_in = os.fdopen(cons_w, "wb", buffering=0)
    os.close(cons_r)  # the segmenter is gone
    os.write(prod_w, _pcm(2000, 8192))
    producer = _FakeProducer()
    _pump_metered(
        producer_out,
        consumer_in,
        producer,  # type: ignore[arg-type]
        tmp_path,
        StreamMeter(16000, 1),
    )
    os.close(prod_w)
    assert producer.terminated


def test_record_marks_alive_within_the_run_not_at_segment_close(
    tmp_path: Path,
) -> None:
    # End to end with a real producer + segmenter: one second of synthetic tone,
    # segments far from closing (60s), NO watchdog — the marker can only have come
    # from the pump measuring the flowing PCM.
    source = AudioSource(
        id="usb",
        name="usb",
        kind=SourceKind.LAVFI,
        spec="sine=frequency=440:sample_rate=16000",
    )
    config = CaptureConfig(sample_rate=16000, channels=1, segment_seconds=60)
    assert record(source, config, tmp_path, max_seconds=1) == 0
    assert (tmp_path / "usb" / ".alive").exists()
    segments = list((tmp_path / "usb").glob("usb-*.opus"))
    assert segments and all(p.stat().st_size > 0 for p in segments)


def test_record_stays_idle_on_digital_silence(tmp_path: Path) -> None:
    # A wedged read delivering pure zeros must NOT light the dot mid-run either.
    source = AudioSource(
        id="usb", name="usb", kind=SourceKind.LAVFI, spec="anullsrc=r=16000:cl=mono"
    )
    config = CaptureConfig(sample_rate=16000, channels=1, segment_seconds=60)
    record(source, config, tmp_path, max_seconds=1)
    assert not (tmp_path / "usb" / ".alive").exists()
