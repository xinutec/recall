"""The recall CLI argument parser — every subcommand and its flags.

Split out of ``recall.cli`` so the ~300 lines of argparse declarations don't sit on
top of the command implementations. ``recall.cli`` imports ``build_parser`` and maps
the parsed ``args.command`` to a handler.
"""

from __future__ import annotations

import argparse
from pathlib import Path

from recall.asr import DEFAULT_MODEL
from recall.finetune import DEFAULT_BASE_MODEL
from recall.llm import DEFAULT_LLM
from recall.stream_server import DEFAULT_INGEST_PORT

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

    rec = sub.add_parser("record", help="capture a source to segment files")
    rec.add_argument("--out", type=Path, default=Path("data"), help="data root")
    rec.add_argument("--id", default="usb", help="source id (filesystem-safe)")
    rec.add_argument(
        "--device",
        default="",
        help="CoreAudio input device name (default: the system default input — "
        "pin it, or a Bluetooth speaker's hands-free mic can take over)",
    )
    rec.add_argument("--seconds", type=int, default=None, help="bounded duration")
    rec.add_argument("--segment-seconds", type=int, default=60)
    rec.add_argument("--sample-rate", type=int, default=48000)
    rec.add_argument("--channels", type=int, default=1)
    rec.add_argument("--codec", default="libopus")
    rec.add_argument("--bitrate", default="32k", help="lossy bitrate, e.g. 32k")

    ver = sub.add_parser("verify", help="report gaps/overlaps in captured segments")
    ver.add_argument("--out", type=Path, default=Path("data"), help="data root")
    ver.add_argument("--id", default="usb", help="source id to verify")
    ver.add_argument("--tolerance-ms", type=int, default=200)

    idx = sub.add_parser("index", help="ingest captured segment metadata")
    idx.add_argument("--out", type=Path, default=Path("data"), help="data root")
    idx.add_argument("--id", default="usb", help="source id to index")

    tra = sub.add_parser("transcribe", help="transcribe captured segments")
    tra.add_argument("--out", type=Path, default=Path("data"), help="data root")
    tra.add_argument("--id", default="usb", help="source id to transcribe")
    tra.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    tra.add_argument(
        "--diarize",
        action="store_true",
        help="per-turn diarized transcription (pyannote; needs HF_TOKEN)",
    )

    rep = sub.add_parser("reprocess", help="re-transcribe with an improved model")
    rep.add_argument("--out", type=Path, default=Path("data"), help="data root")
    rep.add_argument(
        "--model", default=DEFAULT_MODEL, help="mlx model, or a LoRA adapter dir"
    )
    rep.add_argument(
        "--base-model",
        default=DEFAULT_BASE_MODEL,
        help="base for a LoRA adapter --model (HF id)",
    )
    rep.add_argument("--max-confidence", type=float, default=None)

    wrk = sub.add_parser("worker", help="index + transcribe pending audio (one pass)")
    wrk.add_argument("--out", type=Path, default=Path("data"), help="data root")
    wrk.add_argument("--id", default=None, help="source id (default: all sources)")
    wrk.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    wrk.add_argument(
        "--basic", action="store_true", help="whole-clip (skip diarization)"
    )
    wrk.add_argument("--loop", action="store_true", help="run continuously")
    wrk.add_argument("--interval", type=int, default=10, help="loop poll seconds")
    wrk.add_argument(
        "--min-age", type=float, default=30.0, help="skip files newer than this (s)"
    )

    liv = sub.add_parser("live", help="immediate VAD transcription from the mic")
    liv.add_argument("--out", type=Path, default=Path("data"), help="data root")
    liv.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    liv.add_argument(
        "--device",
        default="",
        help="CoreAudio input device name (default: the system default input)",
    )

    cmp = sub.add_parser("compress", help="transcode existing segments to Opus")
    cmp.add_argument("--out", type=Path, default=Path("data"), help="data root")
    cmp.add_argument("--bitrate", default="32k")

    sca = sub.add_parser(
        "score-asr",
        help="transcribe the committed speech fixture (tests/fixtures/speech) with "
        "the real ASR stack and fail if WER drifts past the golden threshold",
    )
    sca.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    sca.add_argument("--base-model", default=DEFAULT_BASE_MODEL)

    smz = sub.add_parser(
        "summarize",
        help="generate per-day summaries with the local LLM (recall layer)",
    )
    smz.add_argument("--out", type=Path, default=Path("data"), help="data root")
    smz.add_argument(
        "--day", help="one day (yyyy-mm-dd); default: all missing complete days"
    )
    smz.add_argument("--llm", default=DEFAULT_LLM, help="mlx-lm instruct model")

    rpb = sub.add_parser(
        "reprobe",
        help="re-measure short-indexed segments (rows caught mid-write) and "
        "repair their recorded duration",
    )
    rpb.add_argument("--out", type=Path, default=Path("data"), help="data root")

    sea = sub.add_parser("search", help="full-text search transcripts")
    sea.add_argument("query", help="FTS query")
    sea.add_argument("--out", type=Path, default=Path("data"), help="data root")
    sea.add_argument("--limit", type=int, default=50)

    sho = sub.add_parser("show", help="inspect specific turns by id (diagnostics)")
    sho.add_argument("ids", nargs="+", type=int, help="transcript id(s)")
    sho.add_argument("--out", type=Path, default=Path("data"), help="data root")

    cov = sub.add_parser(
        "coverage", help="which mics recorded vs transcribed a turn's moment"
    )
    cov.add_argument("id", type=int, help="anchor transcript id")
    cov.add_argument(
        "--window", type=float, default=10.0, help="seconds of context each side"
    )
    cov.add_argument("--out", type=Path, default=Path("data"), help="data root")

    tsc = sub.add_parser(
        "transcript",
        help="read recorded calls/meetings: list sessions or a day's conversations, "
        "then dump one",
    )
    tsc.add_argument("session", nargs="?", help="session id (omit to list sessions)")
    tsc.add_argument(
        "--day", help="instead: a day's conversations — YYYY-MM-DD or 'today'"
    )
    tsc.add_argument(
        "--conv", help="with --day: dump conversation N (a number, or 'last')"
    )
    tsc.add_argument("--out", type=Path, default=Path("data"), help="data root")
    tsc.add_argument("--json", action="store_true", help="machine-readable JSON")

    cor = sub.add_parser(
        "correct",
        help="replace unique substrings in a session's turns with human "
        "corrections (ASR training data); dry-run unless --apply",
    )
    cor.add_argument("session", help="session id")
    cor.add_argument(
        "--fix",
        action="append",
        type=_parse_fix,
        required=True,
        metavar="OLD=>NEW",
        help="replace unique substring OLD with NEW (repeatable)",
    )
    cor.add_argument("--out", type=Path, default=Path("data"), help="data root")
    cor.add_argument(
        "--apply", action="store_true", help="write the corrections (default: dry-run)"
    )

    scan = sub.add_parser(
        "scan-hallucinations",
        help="soft-hide machine turns that land in VAD-detected silence",
    )
    scan.add_argument("--out", type=Path, default=Path("data"), help="data root")

    loops = sub.add_parser(
        "scan-loops", help="soft-hide repetition-loop turns (text check, instant)"
    )
    loops.add_argument("--out", type=Path, default=Path("data"), help="data root")

    rd = sub.add_parser(
        "redrive",
        help="re-transcribe the archive with the current pipeline (supersedes old)",
    )
    rd.add_argument("--out", type=Path, default=Path("data"), help="data root")
    rd.add_argument(
        "--model", default=DEFAULT_MODEL, help="mlx model, or a LoRA adapter dir"
    )
    rd.add_argument(
        "--base-model",
        default=DEFAULT_BASE_MODEL,
        help="base for a LoRA adapter --model (HF id)",
    )
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
    ref.add_argument("--out", type=Path, default=Path("data"), help="data root")
    ref.add_argument(
        "--model", default=DEFAULT_MODEL, help="mlx model, or a LoRA adapter dir"
    )
    ref.add_argument(
        "--base-model",
        default=DEFAULT_BASE_MODEL,
        help="base for a LoRA adapter --model (HF id)",
    )
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

    ing = sub.add_parser(
        "ingest",
        help="single-port audio ingest: phones connect to one port and announce "
        "themselves (replaces a per-device ffmpeg listener each)",
    )
    ing.add_argument("--out", type=Path, default=Path("data"), help="data root")
    ing.add_argument(
        "--port", type=int, default=DEFAULT_INGEST_PORT, help="listen port"
    )

    doc = sub.add_parser(
        "doctor", help="check every recall launchd agent is loaded (health check)"
    )
    doc.add_argument("--out", type=Path, default=Path("data"), help="data root")

    api = sub.add_parser("api", help="serve the FastAPI JSON API + Angular app")
    api.add_argument("--out", type=Path, default=Path("data"), help="data root")
    api.add_argument("--host", default="0.0.0.0", help="bind address")
    api.add_argument("--port", type=int, default=8000)

    exp = sub.add_parser("export-training", help="export corrections as a dataset")
    exp.add_argument("--out", type=Path, default=Path("data"), help="data root")
    exp.add_argument("--dest", type=Path, required=True, help="dataset output dir")

    fit = sub.add_parser("finetune", help="LoRA fine-tune on the corpus (ML env)")
    fit.add_argument("--manifest", type=Path, required=True, help="manifest.jsonl")
    fit.add_argument("--dest", type=Path, required=True, help="adapter output dir")
    fit.add_argument("--base-model", default=DEFAULT_BASE_MODEL)
    fit.add_argument(
        "--epochs", type=int, default=12, help="max epochs (early stop ends sooner)"
    )
    fit.add_argument(
        "--lr", type=float, default=1e-4, help="learning rate (recipe: 1e-4)"
    )
    fit.add_argument("--lora-rank", type=int, default=16)
    fit.add_argument(
        "--eval-holdout",
        type=float,
        default=0.15,
        help="fraction held out for early stopping (0 disables it)",
    )
    fit.add_argument("--early-stopping-patience", type=int, default=2)

    pil = sub.add_parser(
        "finetune-pilot",
        help="train a LoRA on a split of the corpus and report base-vs-adapter WER",
    )
    pil.add_argument("--out", type=Path, default=Path("data"), help="data root")
    pil.add_argument(
        "--dest",
        type=Path,
        default=None,
        help="work dir for corpus + adapter (default: <out>/pilot-finetune)",
    )
    pil.add_argument("--base-model", default=DEFAULT_BASE_MODEL)
    pil.add_argument("--epochs", type=int, default=3)
    pil.add_argument("--lora-rank", type=int, default=16)
    pil.add_argument(
        "--holdout", type=float, default=0.2, help="fraction of clips held out"
    )
    pil.add_argument(
        "--no-pause-capture",
        action="store_true",
        help="don't pause capture during the run (capture is paused by default so "
        "the heavy run can't starve the recorder)",
    )

    att = sub.add_parser(
        "score-attribution",
        help="per-word speaker-attribution accuracy vs a corrected recording",
    )
    att.add_argument("source", help="source id of a recording you've corrected")
    att.add_argument("--out", type=Path, default=Path("data"), help="data root")
    att.add_argument("--model", default=DEFAULT_MODEL, help="mlx-whisper model")
    att.add_argument(
        "--min-turn",
        type=float,
        nargs="*",
        help="min-turn thresholds to sweep (default: 0.3 0.5 0.8 1.2)",
    )

    abc = sub.add_parser(
        "ab-compare",
        help="compare two ASR models on past audio (non-destructive): per-segment "
        "text diff + WER vs your corrections",
    )
    abc.add_argument("--out", type=Path, default=Path("data"), help="data root")
    abc.add_argument("--source", required=True, help="source id of the recording")
    abc.add_argument(
        "--from", dest="frm", default=None, help="ISO start — restrict to a window"
    )
    abc.add_argument("--to", default=None, help="ISO end (with --from)")
    abc.add_argument(
        "--model-a",
        dest="model_a",
        default=DEFAULT_MODEL,
        help="old model: mlx id or LoRA adapter dir (default: base)",
    )
    abc.add_argument(
        "--model-b",
        dest="model_b",
        required=True,
        help="new model: mlx id or LoRA adapter dir",
    )
    abc.add_argument(
        "--base-model",
        default=DEFAULT_BASE_MODEL,
        help="HF base for whichever model is a LoRA adapter",
    )
    abc.add_argument("--limit", type=int, default=100_000)
    abc.add_argument(
        "--report",
        type=Path,
        default=None,
        help="write the markdown report here (default <out>/ab-compare-<source>.md; "
        "a .json is written alongside)",
    )

    enr = sub.add_parser("enroll", help="enroll a speaker voiceprint from audio")
    enr.add_argument("--out", type=Path, default=Path("data"), help="data root")
    enr.add_argument("--name", required=True, help="speaker name")
    enr.add_argument("--audio", type=Path, required=True, help="clean voice clip")
    enr.add_argument("--model", default="pyannote/embedding")

    ide = sub.add_parser("identify", help="resolve speaker turns to enrolled people")
    ide.add_argument("--out", type=Path, default=Path("data"), help="data root")
    ide.add_argument("--model", default="pyannote/embedding")
    ide.add_argument("--threshold", type=float, default=0.5)

    bak = sub.add_parser(
        "backup",
        help="mirror the archive off-machine (consistent DB snapshot + audio)",
    )
    bak.add_argument("--out", type=Path, default=Path("data"), help="data root")
    bak.add_argument(
        "--dest",
        required=True,
        help="rsync destination, e.g. root@odin:/backup/recall-mirror",
    )

    syn = sub.add_parser(
        "sync", help="push the local archive to the fleet (Isis split; needs a token)"
    )
    syn.add_argument("--out", type=Path, default=Path("data"), help="data root")
    syn.add_argument(
        "--url", required=True, help="fleet base URL, e.g. http://10.100.0.2:8000"
    )

    return parser
