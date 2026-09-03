"""The telemetry label is the endpoint's security boundary, not a cosmetic cap.

A label is verbatim UI text written into a log line as ``label=…``. A newline
inside it forges *whole log lines* — including further ``client-event`` lines
attributed to someone else — and the log stops being the evidence it exists to
be.
"""

from __future__ import annotations

from recall.api_client_reports import _one_line


def test_a_label_cannot_forge_a_log_line() -> None:
    forged = "ok\nclient-event kind=tap path=/admin label=Delete everything"
    flat = _one_line(forged, 160)
    assert "\n" not in flat
    assert "\r" not in flat
    assert flat == "ok client-event kind=tap path=/admin label=Delete everything"


def test_the_separators_that_are_not_control_characters_are_flattened() -> None:
    # U+2028 and U+2029 are Zl/Zp rather than Cc, and end a line in some
    # renderers — a check that only looked for \n would miss them.
    assert _one_line("before\u2028after\u2029end", 160) == "before after end"


def test_a_zero_width_character_cannot_hide_inside_a_label() -> None:
    # U+200B is Cf: not whitespace, not a control character, and invisible. A
    # label made of them would read as empty while occupying the whole cap.
    assert _one_line("a\u200bb", 160) == "a b"


def test_an_ordinary_label_is_left_alone() -> None:
    assert _one_line("Pause capture", 160) == "Pause capture"


def test_a_long_label_is_capped() -> None:
    assert len(_one_line("é" * 500, 160)) == 160


def test_a_bidi_override_cannot_disguise_what_the_line_says() -> None:
    # The sharper half of the format-character problem. U+202E flips the
    # rendering of everything after it, so a label can be made to *display* as
    # something other than its content — Trojan Source, aimed at the record
    # rather than at source code, and invisible to whoever reads the log.
    flat = _one_line("Save\u202e\u202dDelete", 160)
    assert "\u202e" not in flat
    assert flat == "Save Delete"
