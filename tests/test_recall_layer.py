"""The recall layer: day summaries + ask-the-archive (generator injected)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.ask import answer_question, build_ask_prompt, retrieve
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.summarize import (
    build_day_prompt,
    build_today_prompt,
    days_needing_summaries,
    refresh_live_summary,
    summarize_day,
)
from recall.timeline import Segment
from recall.vocabulary import build_initial_prompt

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _seeded_store() -> Store:
    store = Store.memory()
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    audio_id = store.add_audio_segment(
        Segment(
            source_id="usb",
            sequence=0,
            start=BASE,
            end=BASE + timedelta(seconds=60),
            path="x",
            sample_rate=48000,
            channels=1,
        )
    )
    for offset, speaker, text in [
        (0, "Alice", "the plumber is coming on Thursday at nine"),
        (5, "Bob", "then I will move the boxes tomorrow"),
        (10, "Alice", "we should buy oranges and coffee"),
    ]:
        store.add_transcript_segment(
            audio_segment_id=audio_id,
            start=BASE + timedelta(seconds=offset),
            end=BASE + timedelta(seconds=offset + 2),
            text=text,
            asr_model="whisper",
            speaker_label=speaker,
        )
    return store


def test_day_prompt_carries_speakers_times_and_text() -> None:
    store = _seeded_store()
    prompt = build_day_prompt(store, "2026-06-13")
    assert prompt is not None
    assert "Alice" in prompt and "Bob" in prompt
    assert "plumber" in prompt
    # Times ground the summary; the prompt renders them in the runner's local tz.
    assert BASE.astimezone().strftime("%H:%M") in prompt
    assert "2026-06-13" in prompt


def test_day_prompt_is_none_for_an_empty_day() -> None:
    store = _seeded_store()
    assert build_day_prompt(store, "2026-01-01") is None


def test_summarize_day_stores_the_generated_summary() -> None:
    store = _seeded_store()
    text = summarize_day(store, lambda _p: "  A short day summary. ", "2026-06-13")
    assert text == "A short day summary."
    assert store.get_day_summary("2026-06-13") == "A short day summary."
    assert store.days_missing_summaries(limit=10) == []


def test_summarize_day_skips_an_empty_day() -> None:
    store = _seeded_store()

    def explode(_p: str) -> str:
        msg = "generator must not run for an empty day"
        raise AssertionError(msg)

    assert summarize_day(store, explode, "2026-01-01") is None
    assert store.get_day_summary("2026-01-01") is None


def test_retrieve_finds_turns_by_question_keywords() -> None:
    store = _seeded_store()
    hits = retrieve(store, "When is the plumber coming?")
    assert any("plumber" in t.text for t in hits)


def test_retrieve_survives_a_question_with_no_matches() -> None:
    store = _seeded_store()
    assert retrieve(store, "did anyone mention zeppelins???") == []


def test_answer_question_cites_the_retrieved_turns() -> None:
    store = _seeded_store()

    def fake_generator(prompt: str) -> str:
        assert "plumber" in prompt  # the retrieved evidence is in the context
        assert "When is the plumber" in prompt  # so is the question
        return "The plumber comes Thursday at nine."

    result = answer_question(store, fake_generator, "When is the plumber coming?")
    assert result is not None
    assert "Thursday" in result.answer
    assert result.sources  # turn ids for deep links
    cited = store.turns_by_id(result.sources)
    assert any("plumber" in t.text for t in cited)


def test_answer_question_declines_without_evidence() -> None:
    store = _seeded_store()

    def explode(_p: str) -> str:
        msg = "generator must not run without evidence"
        raise AssertionError(msg)

    assert answer_question(store, explode, "anything about zeppelins?") is None


def test_ask_prompt_demands_grounding() -> None:
    store = _seeded_store()
    turns = retrieve(store, "plumber")
    prompt = build_ask_prompt("When is the plumber coming?", turns)
    # The prompt must instruct the model to answer ONLY from the excerpts.
    assert "only" in prompt.lower()


def test_days_needing_summaries_excludes_the_still_running_day() -> None:
    # Today's day is incomplete — summarising it mid-afternoon would freeze a
    # partial summary forever (summaries are only regenerated on demand).
    store = _seeded_store()  # turns on 2026-06-13
    assert days_needing_summaries(store, now=BASE + timedelta(days=1)) == ["2026-06-13"]
    assert days_needing_summaries(store, now=BASE) == []  # 06-13 still running
    store.set_day_summary("2026-06-13", "done", model="m")
    assert days_needing_summaries(store, now=BASE + timedelta(days=1)) == []


def test_initial_prompt_carries_names_and_glossary() -> None:
    store = _seeded_store()
    store.enroll_speaker("Griet", [1.0, 0.0], now=BASE)
    store.add_vocabulary_term("Elizabeth Garrett Anderson wing")
    prompt = build_initial_prompt(store)
    assert prompt is not None
    assert "Griet" in prompt
    assert "Elizabeth Garrett Anderson wing" in prompt


def test_initial_prompt_is_none_when_nothing_is_known() -> None:
    assert build_initial_prompt(Store.memory()) is None


def test_initial_prompt_is_capped() -> None:
    store = Store.memory()
    for i in range(200):
        store.add_vocabulary_term(f"vocabularyterm{i:03d}")
    prompt = build_initial_prompt(store)
    assert prompt is not None
    # Whisper's prompt window is ~224 tokens; stay safely under it.
    assert len(prompt) <= 600


def test_today_prompt_says_the_day_is_still_running() -> None:
    # The model must know it's summarising a partial day, so it doesn't write a
    # day-is-over narrative at 10am.
    store = _seeded_store()
    prompt = build_today_prompt(store, "2026-06-13")
    assert prompt is not None
    assert "so far" in prompt
    assert "plumber" in prompt  # carries the same transcript lines
    assert build_today_prompt(store, "2026-01-01") is None


def test_refresh_live_summary_stores_text_with_the_watermark() -> None:
    store = _seeded_store()
    text = refresh_live_summary(
        store, lambda _p: " Morning so far. ", "2026-06-13", model_name="m"
    )
    assert text == "Morning so far."
    row = store.get_live_summary("2026-06-13")
    assert row is not None
    assert row.text == "Morning so far."
    # Watermark = the day's visible-turn state at generation time, so a later
    # request can tell this row is fresh without generating.
    assert row.watermark == store.day_watermark("2026-06-13")


def test_refresh_live_summary_skips_a_day_without_turns() -> None:
    store = _seeded_store()

    def explode(_p: str) -> str:
        msg = "generator must not run for an empty day"
        raise AssertionError(msg)

    assert refresh_live_summary(store, explode, "2026-01-01", model_name="m") is None
    assert store.get_live_summary("2026-01-01") is None


def test_household_context_reaches_every_prompt() -> None:
    """Background facts (stored, not hardcoded) are prepended to the day, today,
    and ask prompts — so the model stops guessing e.g. pronouns — and are clearly
    framed as background, not transcript."""
    store = _seeded_store()
    store.set_setting("household_context", "Rufus is the family dog.")

    day = build_day_prompt(store, "2026-06-13")
    today = build_today_prompt(store, "2026-06-13")
    assert day is not None and "Rufus is the family dog." in day
    assert today is not None and "Rufus is the family dog." in today
    assert "not part of the transcript" in day  # framed as background facts

    def check(prompt: str) -> str:
        return "ok" if "Rufus is the family dog." in prompt else "MISSING"

    result = answer_question(store, check, "plumber")
    assert result is not None and result.answer == "ok"


def test_prompts_omit_the_context_block_when_unset() -> None:
    store = _seeded_store()
    prompt = build_day_prompt(store, "2026-06-13")
    assert prompt is not None
    assert "not part of the transcript" not in prompt


def test_prompts_never_present_voice_guesses_as_names() -> None:
    """A voiceprint GUESS must not reach the LLM as a bare name: out-of-domain
    voices (a guest) match enrolled people at high confidence, and the model
    then asserts the wrong participant (a real bug: a radiographer's turns were
    guessed as an enrolled doctor, and the day summary said he spoke). Same rule
    the sessions list already follows: confirmed labels or nothing."""
    store = _seeded_store()
    audio_id = 1
    turn = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=20),
        end=BASE + timedelta(seconds=24),
        text="the machine will rotate around you",
        asr_model="whisper",
    )
    store.set_speaker_guess(turn, "Griet", 0.95)

    day = build_day_prompt(store, "2026-06-13")
    today = build_today_prompt(store, "2026-06-13")
    ask = build_ask_prompt("what about the machine?", store.turns_by_id([turn]))
    for prompt in (day, today, ask):
        assert prompt is not None
        assert "Griet" not in prompt  # the guess never appears
    assert day is not None
    assert "the machine will rotate" in day  # the text still does
