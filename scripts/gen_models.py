#!/usr/bin/env python3
"""Generate the frontend's API types from the backend's own declarations.

`src/recall/schemas.py` (response TypedDicts) and `src/recall/api_models.py`
(request pydantic models) are the single source of truth for the JSON the API
speaks. This emits the matching `frontend/src/app/models.ts` from them, so the
two ends cannot drift: the verify gate runs `--check` and fails the build if the
committed `models.ts` differs from what the backend would now produce.

  scripts/gen_models.py --write    # regenerate models.ts after a schema change
  scripts/gen_models.py --check    # fail if models.ts is stale (the gate uses this)

No new dependency: pure stdlib introspection of the TypedDicts (FastAPI's OpenAPI
would work too, but this keeps it import-light and exact).
"""

from __future__ import annotations

import difflib
import subprocess
import sys
import types
import typing
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))

from pydantic import BaseModel  # noqa: E402  (after sys.path is set)

from recall import api_models, schemas  # noqa: E402  (after sys.path is set)

FRONTEND = ROOT / "frontend"
OUTPUT = FRONTEND / "src" / "app" / "models.ts"

# Backend TypedDict name -> frontend interface name. Every TypedDict in
# schemas.py must be mapped (a new one without an entry is a hard error, so the
# contract can't silently grow an un-exposed shape).
NAME_MAP = {
    "TranscriptOut": "Transcript",
    "ConversationOut": "Conversation",
    "MomentOut": "Moment",
    "LabelOut": "Label",
    "CaptureOut": "CaptureState",
    "StatusOut": "Status",
    "OkOut": "Ok",
    "ItemsOut": "TranscriptList",
    "PageOut": "TimelinePage",
    "ConversationsOut": "ConversationPage",
    "TrainOut": "TrainQueue",
    "CorrectionsOut": "LabelList",
    "NewIdOut": "CorrectResult",
    "NewIdsOut": "SplitResult",
    "AroundOut": "Around",
    "SuggestOut": "Suggest",
    "SessionOut": "Session",
    "SessionsOut": "SessionList",
    "TranscriptBubbleOut": "TranscriptBubble",
    "TranscriptExportOut": "TranscriptExport",
    "AbCompareScoreOut": "AbCompareScore",
    "AbCompareSegmentDiffOut": "AbCompareSegmentDiff",
    "AbCompareRunSummaryOut": "AbCompareRunSummary",
    "AbCompareRunsOut": "AbCompareRunList",
    "AbCompareRunOut": "AbCompareRun",
    "VocabularyTermOut": "VocabularyTerm",
    "VocabularyOut": "VocabularyList",
    "DaySummaryOut": "DaySummary",
    "DaySummariesOut": "DaySummaryList",
    "TodaySummaryOut": "TodaySummary",
    "ContextOut": "HouseholdContext",
    "AskOut": "AskAnswer",
    "SpeakerNamesOut": "SpeakerNames",
    "AssignResultOut": "AssignResult",
    "VoiceSuggestionsOut": "VoiceSuggestions",
    "QuietSpanOut": "QuietSpan",
    "QuietSpansOut": "QuietSpanList",
    "QuietScanOut": "QuietScan",
    "QuietDeletedOut": "QuietDeleted",
    "EnvelopeSegmentOut": "EnvelopeSegment",
    "SoundEventOut": "SoundEvent",
    "EnvelopeOut": "Envelope",
}

# Request bodies (pydantic models in api_models.py) -> frontend interface name.
# Same rule as NAME_MAP: every model must be mapped, so a new request shape can't
# silently stay un-generated (and hand-duplicated) on the frontend.
REQUEST_NAME_MAP = {
    "ClientLog": "ClientLogRequest",
    "CorrectIn": "CorrectRequest",
    "VoiceNameIn": "VoiceNameRequest",
    "TurnSpeakerIn": "TurnSpeakerRequest",
    "AssignSpanIn": "AssignSpanRequest",
    "UnintelligibleIn": "UnintelligibleRequest",
    "UnhideIn": "UnhideRequest",
    "NudgeIn": "NudgeRequest",
    "RefineRequestIn": "RefineRequest",
    "AbCompareStartIn": "AbCompareStartRequest",
    "ReassignIn": "ReassignRequest",
    "FragmentIn": "SplitFragment",
    "SplitIn": "SplitRequest",
    "AskIn": "AskRequest",
    "VocabularyIn": "VocabularyRequest",
    "SessionRenameIn": "SessionRenameRequest",
    "ContextIn": "ContextRequest",
    "QuietDeleteIn": "QuietDeleteRequest",
    "TelemetryEvent": "TelemetryEvent",
}

# Shapes not emitted to the generated web models: the web app never consumes them
# (fleet liveness/heartbeat — the Kotlin/Swift mic apps read those).
_SKIP = {
    "SourceStatusOut",
    "SourcesOut",
}

_HEADER = (
    "/** The FastAPI backend's API shapes (src/recall/api.py): responses first,\n"
    " *  then request bodies.\n"
    " *\n"
    " *  GENERATED from src/recall/schemas.py + src/recall/api_models.py by\n"
    " *  scripts/gen_models.py — do not edit. Run `scripts/gen_models.py --write`\n"
    " *  after changing a backend shape; the verify gate fails if this file is\n"
    " *  stale. */\n"
)


def _ts_name(typeddict_name: str) -> str:
    try:
        return NAME_MAP[typeddict_name]
    except KeyError:
        raise SystemExit(
            f"gen_models: no frontend name mapped for {typeddict_name!r}; "
            "add it to NAME_MAP"
        ) from None


_PRIMITIVE_TS: dict[object, str] = {
    type(None): "null",
    bool: "boolean",
    int: "number",
    float: "number",
    str: "string",
}


def _ts_element(rendered: str) -> str:
    """Parenthesize a union used as an array element: `[]` binds tighter than `|`, so
    `number | null[]` means something else entirely — `(number | null)[]` is meant."""
    return f"({rendered})" if " | " in rendered else rendered


def _ts_type(tp: object) -> str:
    origin = typing.get_origin(tp)
    if origin is None:
        if tp in _PRIMITIVE_TS:
            return _PRIMITIVE_TS[tp]
        if typing.is_typeddict(tp):
            return _ts_name(typing.cast("type", tp).__name__)
        raise SystemExit(f"gen_models: unhandled type {tp!r}")
    if origin is typing.Literal:
        return " | ".join(f"'{arg}'" for arg in typing.get_args(tp))
    if origin is list:
        (arg,) = typing.get_args(tp)
        return f"readonly {_ts_element(_ts_type(arg))}[]"
    if origin is dict:
        _key, value = typing.get_args(tp)
        return f"Record<string, {_ts_type(value)}>"
    if origin in (types.UnionType, typing.Union):
        return " | ".join(_ts_type(arg) for arg in typing.get_args(tp))
    raise SystemExit(f"gen_models: unhandled type origin {origin!r}")


def _typeddicts() -> list[type]:
    return [
        obj
        for obj in vars(schemas).values()
        if typing.is_typeddict(obj) and obj.__name__ not in _SKIP
    ]


def _request_models() -> list[type[BaseModel]]:
    return [
        obj
        for obj in vars(api_models).values()
        if isinstance(obj, type) and issubclass(obj, BaseModel) and obj is not BaseModel
    ]


def _request_ts_name(model_name: str) -> str:
    try:
        return REQUEST_NAME_MAP[model_name]
    except KeyError:
        raise SystemExit(
            f"gen_models: no frontend name mapped for request model {model_name!r}; "
            "add it to REQUEST_NAME_MAP"
        ) from None


def _ts_request_type(tp: object) -> str:
    # Request models may nest other request models (SplitIn.fragments).
    if isinstance(tp, type) and issubclass(tp, BaseModel):
        return _request_ts_name(tp.__name__)
    origin = typing.get_origin(tp)
    if origin is list:
        (arg,) = typing.get_args(tp)
        return f"readonly {_ts_element(_ts_request_type(arg))}[]"
    if origin in (types.UnionType, typing.Union):
        return " | ".join(_ts_request_type(arg) for arg in typing.get_args(tp))
    return _ts_type(tp)


def render() -> str:
    blocks = [_HEADER]
    for td in _typeddicts():
        hints = typing.get_type_hints(td)
        lines = [f"export interface {_ts_name(td.__name__)} {{"]
        lines += [f"  readonly {field}: {_ts_type(tp)};" for field, tp in hints.items()]
        lines.append("}")
        blocks.append("\n".join(lines))
    blocks.append("// ---- request bodies (POST payloads) ----")
    for model in _request_models():
        hints = typing.get_type_hints(model)
        lines = [f"export interface {_request_ts_name(model.__name__)} {{"]
        for field, info in model.model_fields.items():
            # The wire name (alias) wins — e.g. AbCompareStartIn.frm -> "from".
            name = info.alias or field
            opt = "" if info.is_required() else "?"
            lines.append(f"  readonly {name}{opt}: {_ts_request_type(hints[field])};")
        lines.append("}")
        blocks.append("\n".join(lines))
    return "\n\n".join(blocks) + "\n"


def _prettier(content: str) -> str:
    """Format through the frontend's own prettier so the file is prettier-stable
    (the gate's drift check then compares like for like)."""
    proc = subprocess.run(
        ["npx", "prettier", "--parser", "typescript"],
        input=content,
        capture_output=True,
        text=True,
        cwd=FRONTEND,
        check=False,
    )
    if proc.returncode != 0:
        # Surface prettier's own diagnostic (missing node_modules, parse error) —
        # a bare CalledProcessError hides it and the contract step fails opaquely.
        sys.stderr.write(proc.stderr)
        msg = f"prettier failed with exit {proc.returncode}"
        raise RuntimeError(msg)
    return proc.stdout


def main(argv: list[str]) -> int:
    mode = argv[0] if argv else "--write"
    content = _prettier(render())
    if mode == "--write":
        OUTPUT.write_text(content, encoding="utf-8")
        n_req = len(_request_models())
        print(
            f"wrote {OUTPUT.relative_to(ROOT)} "
            f"({len(_typeddicts())} response + {n_req} request interfaces)"
        )
        return 0
    if mode == "--check":
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.exists() else ""
        if current == content:
            print("models.ts is up to date with the backend schemas")
            return 0
        print(
            "models.ts is STALE — backend schemas changed; "
            "run scripts/gen_models.py --write",
            file=sys.stderr,
        )
        diff = difflib.unified_diff(
            current.splitlines(),
            content.splitlines(),
            fromfile="committed models.ts",
            tofile="generated from schemas.py",
            lineterm="",
        )
        print("\n".join(list(diff)[:40]), file=sys.stderr)
        return 1
    raise SystemExit("usage: gen_models.py [--write|--check]")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
