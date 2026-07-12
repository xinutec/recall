"""Bringing back transcripts a refine pass hid and never replaced."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

from recall.cleanup import HALLUCINATION_REASON, LOOP_REASON
from recall.ids import AudioSegmentId, TranscriptId
from recall.repair import find_blanked, last_generation, restore, retract_into_silence
from recall.sources import AudioSource, SourceKind
from recall.store import HUMAN_MODEL, Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def _tid(i: int) -> TranscriptId:
    return TranscriptId(i)


def test_the_newest_generation_is_the_one_restored() -> None:
    # A segment accretes generations: original, reprocessed, diarized. The last run
    # hidden together is the newest — the best transcript that ever existed for it.
    turns = [
        (_tid(148), "reprocessed (turbo)", "Schrijf dingen op"),
        (_tid(149), "reprocessed (turbo)", "Zoals die recepten"),
        (_tid(5249), "reprocessed (turbo)", "Schrijf dingen op, zoals die recepten"),
        (_tid(6407), "diarized (turbo)", "Schrijf dingen op, zoals die recepten."),
        (_tid(6408), "diarized (turbo)", "En daar wordt het wel opgeslagen."),
    ]
    assert last_generation(turns) == (_tid(6407), _tid(6408))


def test_a_turn_hidden_on_the_evidence_is_never_restored() -> None:
    # Hidden because the VAD heard no speech, or it was a repetition loop: a judgement
    # about what the turn *was*, not a pass replacing it. It stays hidden.
    turns = [
        (_tid(1), "diarized (turbo)", "real speech"),
        (_tid(2), HALLUCINATION_REASON, "Thank you."),
        (_tid(3), LOOP_REASON, "т т т т"),
    ]
    assert last_generation(turns) == (_tid(1),)


def test_a_segment_whose_turns_were_all_hallucinations_is_left_alone() -> None:
    turns = [
        (_tid(1), HALLUCINATION_REASON, "ご視聴ありがとうございました"),
        (_tid(2), LOOP_REASON, "т т т"),
    ]
    assert last_generation(turns) == ()


def _store_with_segment() -> tuple[Store, int]:
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
            path="/archive/usb/seg000.opus",
            sample_rate=48000,
            channels=1,
        )
    )
    return store, int(audio_id)


def test_a_blanked_segment_is_found_and_restored() -> None:
    store, audio_id = _store_with_segment()
    first = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="het is pasta, tagliatelle of wat dan ook",
        asr_model="whisper",
    )
    second = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=4),
        end=BASE + timedelta(seconds=7),
        text="deze keer is het een keer een andere",
        asr_model="whisper",
    )
    # A refine hid them both and wrote nothing in their place: the segment went blank.
    store.hide(first, "reprocessed (turbo)")
    store.hide(second, "reprocessed (turbo)")
    assert store.visible_machine_turns_for_audio(audio_id) == []

    blanked = find_blanked(store)
    assert len(blanked) == 1
    assert "pasta" in blanked[0].preview

    assert restore(store, blanked) == 2
    back = store.visible_machine_turns_for_audio(audio_id)
    assert [t.text for t in back] == [
        "het is pasta, tagliatelle of wat dan ook",
        "deze keer is het een keer een andere",
    ]


def test_a_segment_that_still_shows_a_turn_is_never_touched() -> None:
    # A refine that *did* replace its turns is working correctly. The old ones stay
    # hidden: restoring them would double the transcript.
    store, audio_id = _store_with_segment()
    old = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="rough first pass",
        asr_model="whisper",
    )
    store.hide(old, "diarized (turbo)")
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=3),
        text="the better replacement",
        asr_model="whisper",
    )

    assert find_blanked(store) == []


def test_a_segment_the_detector_heard_nothing_in_gets_nothing_back() -> None:
    """Not every provenance hide was a bug. Sometimes a later pass was correctly
    dropping a hallucination, and restoring that resurrects garbage.

    On the real archive 12 of 170 restorations did exactly that — "E aí", "т т т т" —
    and
    they then blocked the cleanup by making an empty minute look transcribed. The
    detector
    gates the restore, as it gates everything else here.
    """
    store, audio_id = _store_with_segment()
    turn = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="E aí",
        asr_model="whisper",
    )
    store.hide(turn, "reprocessed (turbo)")
    store.set_audio_measurement(AudioSegmentId(audio_id), -64.0, b"\x00\x00")
    store.set_audio_analysis(AudioSegmentId(audio_id), speech_s=0.0, structure=0.4)

    assert find_blanked(store) == []  # nothing to save: there was never any speech


def test_a_hallucination_standing_on_silence_is_retracted() -> None:
    # It is not evidence of speech, it is evidence of an empty room — and it blocks the
    # cleanup, keeping an hour of dead air on the disk to defend a phantom.
    store, audio_id = _store_with_segment()
    store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="ご視聴ありがとうございました",
        asr_model="whisper",
    )
    human = store.add_transcript_segment(
        audio_segment_id=audio_id,
        start=BASE + timedelta(seconds=3),
        end=BASE + timedelta(seconds=4),
        text="a human correction",
        asr_model=HUMAN_MODEL,
    )
    store.set_audio_analysis(AudioSegmentId(audio_id), speech_s=0.0, structure=0.4)

    retracted = retract_into_silence(store)
    assert [text for _id, text in retracted] == ["ご視聴ありがとうございました"]

    # A person's judgement outranks a model's, in both directions: the human turn
    # stands.
    still_there = store.machine_turns_on_silent_audio()
    assert still_there == []  # the machine turn is gone...
    assert human not in [
        int(i) for i, _t in retracted
    ]  # ...and the human one untouched
