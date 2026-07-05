"""Timestamped logging for the long-running agents.

So the capture lifecycle (listen, pause, teardown, resume) and API actions land in
each agent's `.err.log` on the host's **UTC** clock — the same clock the pause logic
uses (`datetime.now(UTC)`), and the one the phone's logcat clock does *not* share.
That makes the host-side timeline self-consistent and diagnosable without fighting
the phone clock skew.
"""

from __future__ import annotations

import logging
import time


def setup() -> None:
    """Configure root logging: INFO, UTC ISO timestamps. Idempotent — basicConfig is
    a no-op once the root logger has a handler, so repeat calls are harmless."""
    logging.Formatter.converter = time.gmtime  # UTC, matching datetime.now(UTC)
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)sZ %(name)s: %(message)s",
        datefmt="%Y-%m-%dT%H:%M:%S",
    )
