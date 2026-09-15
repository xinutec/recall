"""The recall CLI argument parser — every subcommand and its flags.

Split out of ``recall.cli`` so the ~300 lines of argparse declarations don't sit on
top of the command implementations. ``recall.cli`` imports ``build_parser`` and maps
the parsed ``args.command`` to a handler.
"""

from __future__ import annotations

import argparse
from pathlib import Path

from recall.asr import DEFAULT_MODEL
from recall.llm import (
    DEFAULT_IDLE_UNLOAD,
    DEFAULT_LLM,
    LLM_HOST_BIND,
    LLM_HOST_PORT,
)
from recall.paths import default_data_root

_FIX_DELIM = "=>"


def _parse_fix(raw: str) -> tuple[str, str]:
    """Parse a `--fix 'OLD=>NEW'` argument into (old, new)."""
    if _FIX_DELIM not in raw:
        raise argparse.ArgumentTypeError(
            f"--fix must be 'OLD{_FIX_DELIM}NEW', got {raw!r}"
        )
    old, new = (part.strip() for part in raw.split(_FIX_DELIM, 1))
    if not old:
        raise argparse.ArgumentTypeError("the OLD side of a --fix must not be empty")
    return old, new


def build_parser() -> argparse.ArgumentParser:  # noqa: PLR0915 - argparse declarations
    parser = argparse.ArgumentParser(prog="recall")
    sub = parser.add_subparsers(dest="command", required=True)

    tra = sub.add_parser("transcribe", help="transcribe captured segments")
    tra.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    tra.add_argument("--id", default="usb", help="source id to transcribe")
    tra.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    tra.add_argument(
        "--diarize",
        action="store_true",
        help="per-turn diarized transcription (pyannote; needs HF_TOKEN)",
    )

    rep = sub.add_parser("reprocess", help="re-transcribe with an improved model")
    rep.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    rep.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    rep.add_argument("--max-confidence", type=float, default=None)

    sca = sub.add_parser(
        "score-asr",
        help="transcribe the golden speech fixtures present in tests/fixtures/speech "
        "with the real ASR stack and fail if WER drifts past each one's threshold; "
        "the committed clip runs anywhere, the household pair only on this Mac",
    )
    sca.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")

    rpb = sub.add_parser(
        "reprobe",
        help="re-measure short-indexed segments (rows caught mid-write) and "
        "repair their recorded duration",
    )
    rpb.add_argument("--out", type=Path, default=default_data_root(), help="data root")

    scan = sub.add_parser(
        "scan-hallucinations",
        help="soft-hide machine turns that land in VAD-detected silence",
    )
    scan.add_argument("--out", type=Path, default=default_data_root(), help="data root")

    loops = sub.add_parser(
        "scan-loops", help="soft-hide repetition-loop turns (text check, instant)"
    )
    loops.add_argument(
        "--out", type=Path, default=default_data_root(), help="data root"
    )

    script = sub.add_parser(
        "scan-foreign-script",
        help="soft-hide non-Latin-script turns whose audio holds no speech "
        "(Whisper's filler on an empty room is not only English)",
    )
    script.add_argument(
        "--out", type=Path, default=default_data_root(), help="data root"
    )

    wordless = sub.add_parser(
        "scan-wordless",
        help="soft-hide turns with no word in them at all (text check, instant)",
    )
    wordless.add_argument(
        "--out", type=Path, default=default_data_root(), help="data root"
    )

    rd = sub.add_parser(
        "redrive",
        help="re-transcribe the archive with the current pipeline (supersedes old)",
    )
    rd.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    rd.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    rd.add_argument(
        "--limit",
        type=int,
        default=100_000,
        help="max segments this run (chunk it to keep load off capture)",
    )

    ref = sub.add_parser(
        "refine",
        help="diarize-refine the archive while capture is idle (splits merged turns "
        "by speaker; supersedes the basic turns)",
    )
    ref.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    ref.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    ref.add_argument(
        "--llm", default=DEFAULT_LLM, help="mlx-lm model for the summary drain"
    )
    ref.add_argument(
        "--max-segments",
        type=int,
        default=0,
        help="process at most N segments then exit (0 = run as an idle daemon)",
    )
    ref.add_argument(
        "--poll-seconds",
        type=float,
        default=60.0,
        help="how often to re-check for an idle window / new work",
    )
    ref.add_argument(
        "--source",
        default=None,
        help="force a full re-derive of one recording (every segment of this source, "
        "regardless of state) through the canonical pipeline, then exit",
    )

    lmh = sub.add_parser(
        "llm-host",
        help="hold the LLM in ONE process and serve generation on localhost "
        "(recall's summaries/Ask and life's emotion worker share it)",
    )
    lmh.add_argument("--host", default=LLM_HOST_BIND, help="bind address")
    lmh.add_argument("--port", type=int, default=LLM_HOST_PORT)
    lmh.add_argument("--llm", default=DEFAULT_LLM, help="model to hold")
    lmh.add_argument(
        "--idle-unload",
        type=float,
        default=DEFAULT_IDLE_UNLOAD,
        help="seconds of quiet before the weights are released",
    )

    att = sub.add_parser(
        "score-attribution",
        help="per-word speaker-attribution accuracy vs a corrected recording",
    )
    att.add_argument("source", help="source id of a recording you've corrected")
    att.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    att.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    att.add_argument(
        "--min-turn",
        type=float,
        nargs="*",
        help="min-turn thresholds to sweep (default: 0.3 0.5 0.8 1.2)",
    )
    att.add_argument(
        "--max-segments",
        type=int,
        default=None,
        help="stop after scoring this many segments (a whole-archive source like "
        "'usb' replays for hours and only reports at the end; this bounds the run)",
    )
    att.add_argument(
        "--chop",
        type=float,
        default=None,
        help="diarize each segment in independent pieces of this many seconds, as live "
        "capture does to a conversation — chop a meeting at 60 to test whether the "
        "diarization window is what costs the household its boundary accuracy",
    )
    att.add_argument(
        "--clustering-threshold",
        type=float,
        default=None,
        help="override pyannote's clustering threshold (shipped 0.7046, tuned on "
        "meeting corpora). Measured on household audio, the shipped value sits near a "
        "cluster-count MAXIMUM: moving it either way merges (0.55 -> 5, 0.40 -> 2, "
        "0.80 -> 3, 0.90 -> 2 on a 6-cluster segment)",
    )
    att.add_argument(
        "--min-cluster-size",
        type=int,
        default=None,
        help="override pyannote's min_cluster_size (shipped 12, counted in 10s "
        "windows). This is the knob that ADDS clusters (3 -> 8 where 12 gives 6): a "
        "short second speaker in a 60s segment never clears 12, so it is absorbed into "
        "the dominant speaker's cluster — which is a cluster straddling the handover",
    )
    att.add_argument(
        "--while-recording",
        action="store_true",
        help="run even while capture is live. Off by default: this replay runs the "
        "same pyannote+Whisper work refine does, and refine is idle-gated because two "
        "Whispers starve the recorder — lost speech is unrecoverable",
    )
    att.add_argument(
        "--context",
        type=int,
        default=0,
        help="diarize each scored segment together with this many adjacent segments on "
        "each side (never across a recording gap), scoring only the centre — the "
        "opposite of --chop, and the way to test a longer window on live capture",
    )

    enr = sub.add_parser("enroll", help="enroll a speaker voiceprint from audio")
    enr.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    enr.add_argument("--name", required=True, help="speaker name")
    enr.add_argument("--audio", type=Path, required=True, help="clean voice clip")
    enr.add_argument("--model", default="pyannote/embedding")

    ide = sub.add_parser("identify", help="resolve speaker turns to enrolled people")
    ide.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    ide.add_argument("--model", default="pyannote/embedding")
    ide.add_argument("--threshold", type=float, default=0.5)

    syn = sub.add_parser(
        "sync", help="push the local archive to the fleet (Isis split; needs a token)"
    )
    syn.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    syn.add_argument(
        "--url", required=True, help="fleet base URL, e.g. http://10.100.0.2:8000"
    )

    pau = sub.add_parser(
        "pause",
        help="pause recording locally, no network (break-glass when Isis is down)",
    )
    pau.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    pau.add_argument(
        "--minutes",
        type=int,
        default=None,
        help="how long to pause (default: the 24 h safety-net maximum)",
    )

    sub.add_parser(
        "resume",
        help="resume recording locally, no network (break-glass when Isis is down)",
    ).add_argument("--out", type=Path, default=default_data_root(), help="data root")

    job = sub.add_parser(
        "jobs",
        help="run on-demand ML the fleet requested but can't do itself: pull the "
        "fleet's refine queue into this Mac's (Isis split; needs a token)",
    )
    job.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    job.add_argument(
        "--url", required=True, help="fleet base URL, e.g. http://10.100.0.2:8000"
    )

    trace = sub.add_parser(
        "capture-trace",
        help="print the merged capture timeline (events + segments) for loss diagnosis",
    )
    trace.add_argument(
        "--out", type=Path, default=default_data_root(), help="data root"
    )
    trace.add_argument(
        "--minutes", type=int, default=30, help="how far back to look (default 30)"
    )

    rt = sub.add_parser(
        "repair-transcripts",
        help="restore transcripts a refine hid without replacing (dry run by default)",
    )
    rt.add_argument("--out", type=Path, default=default_data_root(), help="data root")
    rt.add_argument(
        "--apply", action="store_true", help="actually restore (default: just report)"
    )

    return parser
