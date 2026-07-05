#!/usr/bin/env python3
"""recall-api — a tiny, dependency-free read client for the recall HTTP API.

Talks to a running recall server (default http://localhost:8000) from anywhere with
python3 — no install, no recall checkout needed beyond this one file. Point it elsewhere
with --base-url or the RECALL_API_URL environment variable.

    recall-api sessions
    recall-api transcript meeting-20260115-1200 --markdown
    recall-api search "birthday" --limit 20

The server runs on the capture host; a remote caller needs a path to it (a tunnel/VPN,
or run this on the host). Read-only: it never changes capture or any data.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from typing import TypedDict, cast

DEFAULT_BASE_URL = os.environ.get("RECALL_API_URL", "http://localhost:8000")


class _Bubble(TypedDict):
    start: str
    speaker: str
    text: str


class _Transcript(TypedDict):
    turns: list[_Bubble]


def fetch(base_url: str, path: str, params: dict[str, object] | None = None) -> object:
    """GET `path` from the API and return the parsed JSON."""
    url = base_url.rstrip("/") + path
    query = {k: v for k, v in (params or {}).items() if v is not None}
    if query:
        url += "?" + urllib.parse.urlencode(query)
    if not url.startswith(("http://", "https://")):
        msg = f"refusing non-http URL: {url}"
        raise ValueError(msg)
    with urllib.request.urlopen(url, timeout=30) as resp:
        return json.load(resp)


def render_markdown(transcript: _Transcript) -> str:
    """A session transcript as markdown bubbles — `**[HH:MM] Speaker:** text`, one blank
    line between speakers. Splice this between markers in a page; it carries no markers
    itself so the surrounding (manually-maintained) content is untouched."""
    blocks = [
        f"**[{turn['start'][11:16]}] {turn['speaker']}:** {turn['text']}"
        for turn in transcript["turns"]
    ]
    return "\n\n".join(blocks) + ("\n" if blocks else "")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="recall-api", description="recall API client")
    parser.add_argument(
        "--base-url", default=DEFAULT_BASE_URL, help="server (default %(default)s)"
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("status", help="capture + recorder status")
    sub.add_parser("capture", help="capture on/off state")
    sub.add_parser("sources", help="recorder liveness")
    sub.add_parser("sessions", help="list recorded sessions")
    sub.add_parser("speakers", help="known speaker names")

    tr = sub.add_parser("transcript", help="a session's clean transcript")
    tr.add_argument("session", help="session id")
    tr.add_argument(
        "--markdown", action="store_true", help="render markdown bubbles, not JSON"
    )

    se = sub.add_parser("search", help="full-text search turns")
    se.add_argument("query")
    se.add_argument("--limit", type=int, default=100)

    tl = sub.add_parser("timeline", help="recent turns (paged)")
    tl.add_argument("--limit", type=int, default=200)
    tl.add_argument("--before", help="ISO timestamp to page back from")

    ar = sub.add_parser("around", help="turns surrounding a turn id")
    ar.add_argument("id", type=int)
    ar.add_argument("-n", type=int, default=2)

    return parser


def _path_and_params(args: argparse.Namespace) -> tuple[str, dict[str, object]]:
    """Map a parsed command to its API path + query params."""
    if args.command == "transcript":
        session = urllib.parse.quote(args.session, safe="")
        return f"/api/sessions/{session}/transcript", {}
    if args.command == "search":
        return "/api/search", {"q": args.query, "limit": args.limit}
    if args.command == "timeline":
        return "/api/timeline", {"limit": args.limit, "before": args.before}
    if args.command == "around":
        return f"/api/around/{args.id}", {"n": args.n}
    # status / capture / sources / sessions / speakers
    return f"/api/{args.command}", {}


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    path, params = _path_and_params(args)
    try:
        data = fetch(args.base_url, path, params)
    except urllib.error.HTTPError as exc:
        sys.stderr.write(f"recall-api: {exc.code} {exc.reason} for {path}\n")
        return 1
    except urllib.error.URLError as exc:
        sys.stderr.write(f"recall-api: cannot reach {args.base_url}: {exc.reason}\n")
        return 1

    if args.command == "transcript" and args.markdown:
        sys.stdout.write(render_markdown(cast("_Transcript", data)))
    else:
        print(json.dumps(data, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
