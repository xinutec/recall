"""The segment-name grammar — the half of capture.py that outlived the recorder.

⚠ The recorder itself is `audiod` (Rust) and its argv is pinned by
`audiod/tests/integration/segmenter.rs`, including the live tap `recall-live`
reads. What is left here is the filename parsing `cli` and `probe` still do.
"""

from __future__ import annotations

from datetime import UTC, datetime

import pytest

from recall.capture import parse_segment_start


def test_parse_segment_start_is_utc() -> None:
    dt = parse_segment_start("usb-20260613T140530.flac")
    assert dt == datetime(2026, 6, 13, 14, 5, 30, tzinfo=UTC)


def test_parse_segment_start_tolerates_dashed_source_id() -> None:
    dt = parse_segment_start("phone-lan-20260613T000000.flac")
    assert dt == datetime(2026, 6, 13, 0, 0, 0, tzinfo=UTC)


def test_parse_segment_start_rejects_garbage() -> None:
    with pytest.raises(ValueError, match="timestamp"):
        parse_segment_start("not-a-segment.flac")
