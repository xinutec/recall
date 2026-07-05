"""Per-day summaries — the first recall-layer view over the archive.

A summary is a derived view like every transcript: regenerable, overwritten on
re-derive, stamped with the model that wrote it (design.md §6). The generator is
injected (recall.llm.Generator), so all of this is testable without a model.

Two kinds: settled summaries (one per finished day, written once by the refine
daemon) and the running day's live summary (a watermarked cache the API refreshes
on demand — regenerated only when new turns landed since the last generation).
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.context import household_context_block
from recall.llm import Generator
from recall.store import Store

# A day can hold thousands of short turns; the prompt takes the most recent slice
# that plausibly fits the context window rather than silently truncating mid-day.
_MAX_TURNS = 400

_PROMPT = """\
{context}You summarise one day of a household's speech transcripts for the people who \
live there. Write 3-6 plain sentences: what happened, decisions made, \
appointments or plans mentioned (with their dates/times), and anything someone \
would want to remember later. Use only the transcript below — do not invent \
details. If it is mostly small talk, say so briefly. Answer in English.

Day: {day}
Transcript (times are local, speaker names where known):
{turns}
"""

# The running day's variant: the model must know the day is incomplete, so it
# narrates what has happened *so far* instead of a day-is-over story at 10am.
_TODAY_PROMPT = """\
{context}You summarise a household's speech transcripts for the people who live there. \
The day is still in progress — the transcript below is what was recorded so \
far today, not a finished day. Write 2-5 plain sentences on what has happened \
so far: decisions made, appointments or plans mentioned (with their \
dates/times), and anything someone would want to remember later. Use only the \
transcript — do not invent details. If it is mostly small talk, say so \
briefly. Answer in English.

Day (so far): {day}
Transcript (times are local, speaker names where known):
{turns}
"""

# Days are grouped by UTC date (matching store.days_missing_summaries); in BST the
# first local hour of a day lands on the previous day's summary — a known, minor
# trade against carrying timezone plumbing through the store.


def _day_bounds(day: str) -> tuple[datetime, datetime]:
    start = datetime.fromisoformat(day).replace(tzinfo=UTC)
    return start, start + timedelta(days=1)


def _turn_lines(store: Store, day: str) -> str | None:
    """The day's visible turns as prompt lines, or None if it has none."""
    start, end = _day_bounds(day)
    turns = store.segments_in_range(start, end)
    if not turns:
        return None
    # Confirmed labels only — never voiceprint guesses: an out-of-domain voice
    # (a guest) matches enrolled people at high confidence, and a guessed name in
    # the prompt becomes an asserted participant in the summary. Same rule as the
    # sessions list.
    return "\n".join(
        f"[{t.start.astimezone().strftime('%H:%M')}] {t.speaker_label or '?'}: {t.text}"
        for t in turns[-_MAX_TURNS:]
    )


def build_day_prompt(store: Store, day: str) -> str | None:
    """The summarisation prompt for `day` (yyyy-mm-dd), or None if it has no
    visible turns."""
    lines = _turn_lines(store, day)
    if lines is None:
        return None
    return _PROMPT.format(context=household_context_block(store), day=day, turns=lines)


def build_today_prompt(store: Store, day: str) -> str | None:
    """The running day's so-far prompt, or None if nothing was recorded yet."""
    lines = _turn_lines(store, day)
    if lines is None:
        return None
    return _TODAY_PROMPT.format(
        context=household_context_block(store), day=day, turns=lines
    )


def days_needing_summaries(store: Store, *, now: datetime) -> list[str]:
    """Complete days (strictly before `now`'s UTC date) with turns but no summary
    — the idle drain's work-list. The running day is excluded: a mid-day summary
    would freeze partial and stale (its live view is refresh_live_summary)."""
    today = now.astimezone(UTC).strftime("%Y-%m-%d")
    return [d for d in store.days_missing_summaries() if d < today]


def summarize_day(
    store: Store, generator: Generator, day: str, *, model_name: str = "unknown"
) -> str | None:
    """Generate and store `day`'s summary; None (and nothing stored) for a day
    with no visible turns."""
    prompt = build_day_prompt(store, day)
    if prompt is None:
        return None
    text = generator(prompt).strip()
    store.set_day_summary(day, text, model=model_name)
    return text


def refresh_live_summary(
    store: Store, generator: Generator, day: str, *, model_name: str = "unknown"
) -> str | None:
    """Regenerate the running day's so-far summary and cache it with its
    freshness watermark; None (and nothing stored) if nothing was recorded yet.

    The watermark is read *before* generating: a turn that lands during the
    (slow) generation leaves the stored row stale, so the next request
    correctly triggers another refresh rather than trusting a summary that
    missed it."""
    watermark = store.day_watermark(day)
    if watermark is None:
        return None
    prompt = build_today_prompt(store, day)
    if prompt is None:
        return None
    text = generator(prompt).strip()
    store.set_live_summary(day, text, model=model_name, watermark=watermark)
    return text
