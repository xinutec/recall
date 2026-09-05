"""Assemble a continuous window from recalld's room blocks, for the referee.

The WER bake-off (`fusion_bakeoff.py`) judges one continuous `--fused` file
against the reference microphone. The room stream exists as one FLAC per
UTC-aligned minute on Isis (docs/architecture.md, stage D3), so this fetches
each block of a window over the read plane and lays it at its grid position
in one 16 kHz mono WAV — the referee itself stays untouched, which is what
keeps its verdicts comparable with every earlier run.

A minute the builder did not build is laid down as silence and COUNTED: the
count is printed at the end and a wholly-missing window is an error, because
"the room was quiet" and "the blocks are not built yet" must never read the
same.

    .venv/bin/python scripts/room_window.py \
        --url http://10.100.0.2:8001 --start 2026-06-23T20:12:00Z \
        --minutes 30 --out /tmp/room-window.wav

The read token comes from RECALL_SYNC_TOKEN (the read side is the Mac's
plane), never argv.
"""

from __future__ import annotations

import argparse
import os
import struct
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from datetime import UTC, datetime, timedelta
from pathlib import Path

RATE = 16_000
HTTP_NOT_FOUND = 404
BLOCK_S = 60
BLOCK_SAMPLES = RATE * BLOCK_S


def _utc(text: str) -> datetime:
    return datetime.fromisoformat(text.replace("Z", "+00:00")).astimezone(UTC)


def fetch_block(base: str, token: str | None, stamp: str) -> bytes | None:
    """One room block's FLAC bytes, or None where no block was built."""
    request = urllib.request.Request(f"{base}/ingest/v1/blob/room/room-{stamp}.flac")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return bytes(response.read())
    except urllib.error.HTTPError as err:
        if err.code == HTTP_NOT_FOUND:
            return None
        raise


def decode_block(flac: bytes) -> bytes:
    """FLAC to exactly one minute of s16le mono 16 kHz, padded or trimmed."""
    with tempfile.NamedTemporaryFile(suffix=".flac") as tmp:
        tmp.write(flac)
        tmp.flush()
        out = subprocess.run(
            [
                "ffmpeg",
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                tmp.name,
                "-ac",
                "1",
                "-ar",
                str(RATE),
                "-f",
                "s16le",
                "-",
            ],
            check=True,
            capture_output=True,
        ).stdout
    want = BLOCK_SAMPLES * 2
    return out[:want].ljust(want, b"\x00")


def write_wav(path: Path, pcm: bytes) -> None:
    with path.open("wb") as out:
        out.write(b"RIFF")
        out.write(struct.pack("<I", 36 + len(pcm)))
        out.write(b"WAVEfmt ")
        out.write(struct.pack("<IHHIIHH", 16, 1, 1, RATE, RATE * 2, 2, 16))
        out.write(b"data")
        out.write(struct.pack("<I", len(pcm)))
        out.write(pcm)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument(
        "--start", required=True, help="window start, RFC3339, minute-aligned"
    )
    parser.add_argument("--minutes", type=int, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    start = _utc(args.start)
    if start.second or start.microsecond:
        parser.error(
            "--start must be minute-aligned: room blocks live on the UTC minute grid"
        )
    token = os.environ.get("RECALL_SYNC_TOKEN")

    pcm = bytearray()
    missing: list[str] = []
    for i in range(args.minutes):
        stamp = (start + timedelta(minutes=i)).strftime("%Y%m%dT%H%M%S")
        flac = fetch_block(args.url, token, stamp)
        if flac is None:
            missing.append(stamp)
            pcm.extend(b"\x00" * (BLOCK_SAMPLES * 2))
        else:
            pcm.extend(decode_block(flac))
    write_wav(args.out, bytes(pcm))

    built = args.minutes - len(missing)
    print(f"{built}/{args.minutes} blocks built; {len(missing)} laid as silence")
    for stamp in missing:
        print(f"  missing: room-{stamp}.flac", file=sys.stderr)
    if built == 0:
        sys.exit("no room blocks exist in this window — nothing to judge")


if __name__ == "__main__":
    main()
