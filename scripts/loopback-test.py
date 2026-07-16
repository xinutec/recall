#!/usr/bin/env python3
"""Acoustic-loopback test of the capture path (docs/capture-loss-plan.md Phase 4).

Drives the REAL production control path — resume via the fleet's
/api/capture/resume (Isis intent -> capture-mirror -> Mac agents + phone
listeners) — then plays a distinctive nonce phrase through a speaker on the
capture host, re-pauses, and judges which sources actually received audio at
what level: phones from their durable ingest telemetry (capture_events),
the USB mic by decoding the segment files it wrote (its PCM never passes
through Python, so there is no in-line meter for it). With --transcript it
also waits for the worker to prove the words survived end to end into
transcript_segments.

Makes "the short flow records" a testable property instead of a thing
re-discovered by hand. Run ON the capture host from the repo devshell (it
plays audio via `say` and decodes with `ffmpeg`). Leaves capture as it found
it: a paused household is re-paused (bounded, the API's only pause form).

    scripts/loopback-test.py --speaker "Bose Revolve SoundLink" --expect usb

Exit code: 0 when every --expect source heard the phrase (telemetry tier; the
transcript tier too when --transcript is given), 1 otherwise.
"""

from __future__ import annotations

import argparse
import json
import math
import random
import sqlite3
import subprocess
import sys
import time
import urllib.request
from array import array
from datetime import UTC, datetime
from pathlib import Path

# Distinctive, easily-transcribed words; two make the phrase unique per run so a
# transcript match can never be satisfied by an earlier run's audio.
_NONCE_WORDS = [
    "walrus",
    "gramophone",
    "thermodynamics",
    "marmalade",
    "telescope",
    "asparagus",
    "zeppelin",
    "harpsichord",
    "mandolin",
    "porcupine",
]

# Telemetry pass bar: a phrase played through a room speaker lands well above
# this at any working mic (last verified: -13 dB next to the phone, -44..-56 dB
# room floor); a dead path shows -90 dB or no bytes at all.
_MIN_PEAK_DB = -40.0


def _say(message: str) -> None:
    """Timestamped, unbuffered progress line — the test's own timing is part of the
    diagnosis (e.g. `say` blocking for a minute while a sleeping speaker wakes)."""
    print(f"{datetime.now(UTC):%H:%M:%S}Z {message}", flush=True)


def _speaker_id(speaker: str) -> str:
    """Resolve a `say -a` output device: numeric ids pass through, names are looked
    up in `say -a '?'`. Names must be resolved to ids — passing a device NAME to
    `say -a` crashes outright (NSRangeException) on current macOS."""
    if speaker.isdigit():
        return speaker
    listing = subprocess.run(
        ["say", "-a", "?"], check=True, capture_output=True, text=True
    ).stdout
    for line in listing.splitlines():
        ident, _, name = line.strip().partition(" ")
        if name.strip() == speaker:
            return ident
    msg = f"speaker {speaker!r} not found in `say -a '?'`:\n{listing}"
    raise SystemExit(msg)


def _api(fleet: str, path: str, *, post: bool = False) -> dict[str, object]:
    req = urllib.request.Request(f"{fleet}/api{path}", method="POST" if post else "GET")
    with urllib.request.urlopen(req, timeout=10) as resp:
        data: dict[str, object] = json.loads(resp.read())
        return data


def _events_since(db: Path, since: datetime) -> list[tuple[str, str, str | None]]:
    """(kind, source_id, detail) capture events since `since`, oldest-first."""
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT kind, source_id, detail FROM capture_events "
            "WHERE utc >= ? ORDER BY utc, id",
            (since.isoformat(),),
        ).fetchall()
    finally:
        conn.close()
    return [(str(k), str(s or ""), d) for k, s, d in rows]


def _transcribed_sources(db: Path, since: datetime, nonce: str) -> set[str]:
    """Sources whose transcript since `since` contains the nonce word."""
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT DISTINCT a.source_id FROM transcript_segments t "
            "JOIN audio_segments a ON a.id = t.audio_segment_id "
            "WHERE t.start_utc >= ? AND t.text LIKE ?",
            (since.isoformat(), f"%{nonce}%"),
        ).fetchall()
    finally:
        conn.close()
    return {str(r[0]) for r in rows}


def _ingest_peaks(events: list[tuple[str, str, str | None]]) -> dict[str, float]:
    """Best peak dB per source across this run's ingest_disconnect events."""
    peaks: dict[str, float] = {}
    for kind, source_id, detail in events:
        if kind != "ingest_disconnect" or not detail:
            continue
        try:
            peak = json.loads(detail).get("peak_db")
        except ValueError:
            continue
        if isinstance(peak, int | float):
            peaks[source_id] = max(peaks.get(source_id, -120.0), float(peak))
    return peaks


def _file_peak_db(path: Path) -> float | None:
    """Peak dBFS of a segment file, by decoding it (for meterless sources: usb)."""
    pcm = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-f", "s16le", "-ac", "1", "-"],
        check=True,
        capture_output=True,
    ).stdout
    pcm = pcm[: len(pcm) - (len(pcm) % 2)]
    if not pcm:
        return None
    samples = array("h", pcm)
    low, high = min(samples), max(samples)
    peak = max(high, -low)
    return 20 * math.log10(peak / 32768) if peak else None


def _segment_peak(out: Path, source_id: str, since: datetime) -> float | None:
    """Best peak dB across the segment files `source_id` wrote since `since`."""
    best: float | None = None
    for path in (out / source_id).glob(f"{source_id}-*"):
        try:
            stat = path.stat()
        except OSError:
            continue
        if stat.st_mtime < since.timestamp() or stat.st_size == 0:
            continue
        peak = _file_peak_db(path)
        if peak is not None and (best is None or peak > best):
            best = peak
    return best


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fleet", default="http://10.100.0.2:8000")
    parser.add_argument("--out", type=Path, default=Path("/Volumes/Backup/recall"))
    parser.add_argument("--speaker", required=True, help="`say -a` output device name")
    parser.add_argument(
        "--expect",
        action="append",
        default=[],
        help="source id that must hear the phrase (repeatable)",
    )
    parser.add_argument(
        "--pre",
        type=float,
        default=20.0,
        help="seconds after resume before speaking (mirror+agents+phones settle)",
    )
    parser.add_argument(
        "--tail",
        type=float,
        default=5.0,
        help="seconds of recording kept after the phrase before re-pausing",
    )
    parser.add_argument(
        "--transcript",
        action="store_true",
        help="also wait for the phrase to appear in transcript_segments",
    )
    parser.add_argument(
        "--transcript-timeout",
        type=float,
        default=600.0,
        help="seconds to wait for the transcript tier",
    )
    args = parser.parse_args()

    db = args.out / "recall.sqlite"
    nonce = " ".join(random.sample(_NONCE_WORDS, 2))
    phrase = f"recall loopback marker, {nonce}, over"
    started = datetime.now(UTC)

    speaker = _speaker_id(args.speaker)
    state = _api(args.fleet, "/capture")
    was_paused = not state.get("running", True)
    _say(f"capture: {'paused' if was_paused else 'running'}; phrase: {phrase!r}")

    if was_paused:
        _api(args.fleet, "/capture/resume", post=True)
        _say(f"resume posted; waiting {args.pre:.0f}s for the chain to settle")
    try:
        time.sleep(args.pre)
        _say("speaking")
        subprocess.run(["say", "-a", speaker, phrase], check=True)
        _say("say returned")  # a sleeping BT speaker can block this for a minute
        time.sleep(args.tail)
    finally:
        # A paused household MUST come back paused even when the test dies mid-run —
        # this restore is the one thing the script is never allowed to skip.
        if was_paused:
            _api(args.fleet, "/capture/pause", post=True)
            _say("pause posted (bounded, as before)")
    # Phones disconnect when the listener closes; give their events time to land.
    time.sleep(12.0)

    events = _events_since(db, started)
    _say(f"{len(events)} capture events since {started:%H:%M:%S}Z:")
    for kind, source_id, detail in events:
        print(f"  {kind:<18} {source_id}{'  ' + detail if detail else ''}")

    failed = _telemetry_failures(args.out, args.expect, events, started)
    if args.transcript:
        failed += _transcript_failures(
            db, args.expect, started, nonce, timeout=args.transcript_timeout
        )

    if failed:
        print(f"FAIL: {sorted(set(failed))}")
        return 1
    print("PASS")
    return 0


def _telemetry_failures(
    out: Path,
    expect: list[str],
    events: list[tuple[str, str, str | None]],
    started: datetime,
) -> list[str]:
    """Expected sources whose measured level says they did NOT hear the phrase."""
    ingest_peaks = _ingest_peaks(events)
    failed: list[str] = []
    for source_id in expect:
        peak = ingest_peaks.get(source_id)
        via = "ingest"
        if peak is None:  # meterless source (usb): judge from its segment files
            peak = _segment_peak(out, source_id, started)
            via = "segment file"
        ok = peak is not None and peak >= _MIN_PEAK_DB
        level = f"{peak:.1f} dB" if peak is not None else "no audio"
        print(
            f"telemetry {source_id}: {'HEARD' if ok else 'NOT HEARD'} ({level}, {via})"
        )
        if not ok:
            failed.append(source_id)
    return failed


def _transcript_failures(
    db: Path, expect: list[str], started: datetime, nonce: str, *, timeout: float
) -> list[str]:
    """Expected sources on which the nonce never reached the transcript."""
    word = nonce.split(maxsplit=1)[0]
    deadline = time.monotonic() + timeout
    transcribed: set[str] = set()
    while time.monotonic() < deadline:
        transcribed = _transcribed_sources(db, started, word)
        if set(expect) <= transcribed:
            break
        time.sleep(15.0)
    print(f"transcribed on: {sorted(transcribed) or 'nothing'}")
    return [s for s in expect if s not in transcribed]


if __name__ == "__main__":
    sys.exit(main())
