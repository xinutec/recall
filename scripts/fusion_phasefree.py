"""Per-bin selection across microphones — the phase-free fusion experiment (#1522).

The bakeoff (fusion_bakeoff.py) refuted AVERAGING on phase-destroyed Opus: a good
mic fused with worse ones was dragged down. This script tests the routes that do
not need phase: rank the mics, then rebuild the window taking each time-frequency
BIN (magnitude AND phase together) from whichever mic has the best local SNR
there. Each device's Opus artefacts are independent, so a bin one encoder mangled
is often intact on another — selection dodges what summation smears in.

Alignment is envelope cross-correlation against the reference, one global offset
per source. That is frame-level (~21 ms at 1024/48k), which is all selection
needs; sub-sample alignment only matters for coherent summation, which is dead on
this input. Clock drift over a 12-minute window is <=14 ms at 20 ppm — under one
frame, so it is deliberately not corrected here.

Outputs land OUTSIDE the repo (never commit audio): a per-mic aligned WAV, the
best mic as-is, the per-bin selection, and a JSON report of ranking and offsets.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
import subprocess
import tempfile
from datetime import UTC, datetime, timedelta
from pathlib import Path

import numpy as np
from numpy.typing import NDArray
from scipy.io import wavfile
from scipy.signal import fftconvolve, istft, stft

RATE = 48_000
ENV_HOP = 480  # 10 ms envelope resolution
SEARCH_S = 5.0  # alignment search radius
NFFT = 1024
HOP = 256
FLOOR_PCT = 10.0  # per-(mic, band) noise floor percentile


def _utc(text: str) -> datetime:
    return datetime.fromisoformat(text.replace("Z", "+00:00")).astimezone(UTC)


def decode(src: Path, out: Path) -> None:
    """Whole file as 48 kHz mono f32 WAV — selection wants full bandwidth."""
    subprocess.run(
        [
            "ffmpeg",
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            str(src),
            "-ac",
            "1",
            "-ar",
            str(RATE),
            "-c:a",
            "pcm_f32le",
            str(out),
        ],
        check=True,
    )


def load_track(
    db: Path, source: str, start: datetime, end: datetime, tmp: Path
) -> NDArray[np.float32] | None:
    """One source's window as a single buffer, segments placed by wall clock."""
    rows = (
        sqlite3.connect(f"file:{db}?mode=ro", uri=True)
        .execute(
            """
        select a.path, a.start_utc from audio_segments a
        join sources s on s.id = a.source_id
        where s.name = ? and a.start_utc >= ? and a.start_utc < ?
        order by a.start_utc
        """,
            (source, start.isoformat(), end.isoformat()),
        )
        .fetchall()
    )
    if not rows:
        return None
    buf = np.zeros(int((end - start).total_seconds() * RATE), dtype=np.float32)
    for path, seg_start in rows:
        wav = tmp / (Path(path).stem + ".wav")
        decode(Path(path), wav)
        _, data = wavfile.read(wav)
        at = int((_utc(seg_start) - start).total_seconds() * RATE)
        n = min(len(data), len(buf) - at)
        if n > 0:
            buf[at : at + n] = data[:n]
    return buf


def envelope(x: NDArray[np.float32]) -> NDArray[np.float64]:
    frames = x[: len(x) - len(x) % ENV_HOP].reshape(-1, ENV_HOP)
    rms: NDArray[np.float64] = np.sqrt(np.mean(frames.astype(np.float64) ** 2, axis=1))
    return rms


def align_offset(ref: NDArray[np.float32], other: NDArray[np.float32]) -> int:
    """Samples to shift `other` by, from envelope cross-correlation (phase-free)."""
    a, b = envelope(ref), envelope(other)
    a = a - a.mean()
    b = b - b.mean()
    corr = fftconvolve(a, b[::-1])
    radius = int(SEARCH_S * RATE / ENV_HOP)
    centre = len(b) - 1
    window = corr[centre - radius : centre + radius + 1]
    return (int(np.argmax(window)) - radius) * ENV_HOP


def shift(x: NDArray[np.float32], by: int) -> NDArray[np.float32]:
    out = np.zeros_like(x)
    if by >= 0:
        out[by:] = x[: len(x) - by]
    else:
        out[: len(x) + by] = x[-by:]
    return out


def speech_snr_db(x: NDArray[np.float32]) -> float:
    """Rank metric: active-to-floor ratio of the 300 Hz-4 kHz envelope."""
    _, _, spec = stft(x, fs=RATE, nperseg=NFFT, noverlap=NFFT - HOP)
    lo, hi = int(300 * NFFT / RATE), int(4000 * NFFT / RATE)
    band = np.abs(spec[lo:hi]).mean(axis=0)
    alive = band > 1e-4 * band.max()
    floor = float(np.percentile(band[alive], FLOOR_PCT)) if alive.any() else 1e-12
    active = float(np.percentile(band, 90.0))
    return float(20 * np.log10(active / max(floor, 1e-12)))


def per_bin_select(
    tracks: dict[str, NDArray[np.float32]],
) -> tuple[NDArray[np.float32], NDArray[np.float64]]:
    """Each time-frequency bin from the mic with the best local SNR there.

    Magnitude and phase travel together (per-bin channel selection), so no
    cross-mic phase is ever combined. Local SNR is magnitude over that mic's own
    per-frequency noise floor, which normalises gain differences between devices.
    Returns the selected signal and the fraction each mic won, keyed like tracks.
    """
    names = sorted(tracks)
    specs = []
    for name in names:
        _, _, spec = stft(tracks[name], fs=RATE, nperseg=NFFT, noverlap=NFFT - HOP)
        specs.append(spec)
    mags = np.stack([np.abs(s) for s in specs])
    # Equalise by each mic's speech-ACTIVE level, then select on absolute
    # magnitude. A floor-based ratio is refuted for this fleet: a phone-side
    # denoiser (geb, Sep 4) scrubs its own floor to near zero and wins every
    # bin with an unintelligible stream — twice, v1 and the gate-proof v2
    # floor both. The 95th percentile is made of real signal, which a
    # denoiser cannot inflate, and its scrubbed zeros can never win a bin.
    level = np.percentile(mags, 95.0, axis=(1, 2), keepdims=True)
    winner = np.argmax(mags / np.maximum(level, 1e-12), axis=0)
    stacked = np.stack(specs)
    selected = np.take_along_axis(stacked, winner[None], axis=0)[0]
    _, out = istft(selected, fs=RATE, nperseg=NFFT, noverlap=NFFT - HOP)
    shares = np.bincount(winner.ravel(), minlength=len(names)) / winner.size
    return out.astype(np.float32), shares


def write_wav(path: Path, x: NDArray[np.float32]) -> None:
    peak = float(np.max(np.abs(x)))
    if peak > 0:
        x = (x / peak * 0.9).astype(np.float32)
    wavfile.write(path, RATE, x)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, required=True)
    parser.add_argument("--start", required=True, help="window start, RFC3339")
    parser.add_argument("--minutes", type=int, required=True)
    parser.add_argument("--sources", nargs="+", required=True)
    parser.add_argument("--reference", default="usb")
    parser.add_argument(
        "--out", type=Path, required=True, help="directory, not in the repo"
    )
    args = parser.parse_args()

    start = _utc(args.start)
    end = start + timedelta(minutes=args.minutes)
    args.out.mkdir(parents=True, exist_ok=True)

    tracks: dict[str, NDArray[np.float32]] = {}
    with tempfile.TemporaryDirectory(prefix="phasefree-") as tmp:
        for name in args.sources:
            track = load_track(args.db, name, start, end, Path(tmp))
            if track is None:
                print(f"{name}: nothing in the window, skipped")
                continue
            tracks[name] = track

    ref = tracks[args.reference]
    offsets: dict[str, int] = {}
    for name, track in tracks.items():
        offsets[name] = 0 if name == args.reference else align_offset(ref, track)
        tracks[name] = shift(track, offsets[name])

    ranking = {name: speech_snr_db(track) for name, track in tracks.items()}
    best = max(ranking, key=lambda name: ranking[name])

    selected, shares = per_bin_select(tracks)
    names = sorted(tracks)

    for name, track in tracks.items():
        write_wav(args.out / f"aligned-{name.replace(' ', '')}.wav", track)
    write_wav(args.out / "best-mic.wav", tracks[best])
    write_wav(args.out / "selected.wav", selected)

    report = {
        "window": {"start": start.isoformat(), "minutes": args.minutes},
        "offsets_ms": {n: round(o / RATE * 1000, 1) for n, o in offsets.items()},
        "snr_db": {n: round(v, 1) for n, v in ranking.items()},
        "best": best,
        "win_share": {
            n: round(float(s), 3) for n, s in zip(names, shares, strict=True)
        },
    }
    (args.out / "report.json").write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
