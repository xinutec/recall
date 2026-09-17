"""Command-line entry point: `python -m recall record|verify|index|search`.

`record` captures a source to segment files (needs the mic + macOS permission).
`verify` scans captured segments and reports gaps/overlaps (Phase 0 check).
`index` ingests captured segments' metadata into the store.
`search` full-text-searches transcripts in the store.
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
from contextlib import ExitStack
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall import capture_control, runlog
from recall.asr import (
    AsrResult,
    Transcriber,
    Word,
    concat_working_copy,
    mlx_transcribe,
)
from recall.attribution import AttributionReport, context_window
from recall.capture import parse_segment_start, segment_glob
from recall.cleanup import (
    scan_empty_text,
    scan_foreign_script,
    scan_hallucinations,
    scan_loops,
)
from recall.cli_parser import build_parser
from recall.diarize import SpeakerTurn, pyannote_diarize
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.logrotate import rotate_logs
from recall.maintenance import (
    reprobe_short_segments,
)
from recall.paths import ArchiveAway, require_archive
from recall.probe import probe_media, scan_segments
from recall.redrive import redrive_archive
from recall.reprocess import reprocess
from recall.sources import AudioSource, SourceKind
from recall.store import Store
from recall.vad import silero_speech_regions
from recall.vocabulary import build_initial_prompt
from recall.wer import word_error_rate

# Per worker pass, how many turns to measure loudness for. Bounded so the sox
# decode loop drains the backlog gradually without starving capture.
_LOUDNESS_BACKFILL_PER_PASS = 100
# Smoothing threshold the attribution breakdown is reported at (the sweep showed it
# barely matters, so any in-range value gives the same localisation).
_REF_MIN_TURN = 0.5
# Per pass, how many human-corrected turns to align to ASR for word timings. Each is a
# word-level Whisper pass, so keep it small next to live capture.
_WORD_TIMINGS_BACKFILL_PER_PASS = 3
# Per pass, how many speaker-tagged corrections to embed into voiceprints. Small:
# embedding loads pyannote and must stay light next to live capture.
_VOICEPRINT_BACKFILL_PER_PASS = 8
# Per pass, how many un-embedded machine turns to embed (once). Each is a pyannote
# embedding, so keep it light next to live capture.
_EMBED_BACKFILL_PER_PASS = 16
# The agents' log directory, bounded each worker pass so it can't grow without limit.
# An absolute path rather than one derived from __file__: the agents run this source
# from a read-only store copy (deploy/hm-agents.nix), where "next to the code" is both
# the wrong place and unwritable — rotation would quietly stop. RECALL_LOG_DIR
# overrides it.
_LOG_DIR = Path(os.environ.get("RECALL_LOG_DIR") or Path.home() / "Library/Logs/recall")


def _db_path(root: Path) -> Path:
    return root / "recall.sqlite"


def _source_found_on_disk(source_id: str) -> AudioSource:
    """A source id the CLI was handed for audio that is already in the data root.

    DISCOVERED, not COREAUDIO — the same answer `process_all` gives (worker.py). These
    commands did not record the audio and nothing about a directory says what did, and
    `add_source` is INSERT OR IGNORE, so a guess made here sticks until an authoritative
    registrar corrects it (`Store.register_source`). Guessing COREAUDIO filed a meeting
    copied into the data root as a microphone: gone from the sessions list, health-
    checked as a mic that had stopped, and — the cost that is not recoverable —
    SWEEPABLE, so the quiet review was entitled to delete it.

    One helper rather than a literal per call site: the bug was three constructions of
    the same thing drifting from the one in worker.py.
    """
    return AudioSource(
        id=source_id, name=source_id, kind=SourceKind.DISCOVERED, spec=""
    )


def capture_is_idle(out: Path) -> bool:
    """Is the recorder parked right now? A live pause marker is the one signal the
    capture agents themselves gate on, so this reads the same truth they do."""
    return capture_control.is_paused(out, datetime.now(UTC))


def recording_refusal(out: Path, *, allow: bool) -> str | None:
    """Why a heavy offline pass must not start, or None if it may.

    The evaluation passes run the same pyannote + Whisper work `refine` does, and refine
    only runs while capture is paused because two Whispers starve the recorder (sox
    buffer overrun = dropped samples). Completeness is requirement #1 and lost audio is
    unrecoverable, so the default is to refuse rather than to compete; `allow` is the
    deliberate override, never an inferred fallback.
    """
    if allow or capture_is_idle(out):
        return None
    return (
        "capture is recording — this pass would compete with it for the GPU and can "
        "cost speech (two Whispers starve the recorder). Pause first with "
        "`recall pause --minutes N`, or pass --while-recording to run anyway."
    )


def _cmd_transcribe(args: argparse.Namespace) -> int:
    source = _source_found_on_disk(args.id)
    segments = scan_segments(args.out / args.id, args.id)
    store = Store.open(_db_path(args.out))

    def transcriber(audio: Path) -> AsrResult:
        return mlx_transcribe(
            audio, model=args.model, initial_prompt=build_initial_prompt(store)
        )

    try:
        store.add_source(source)
        if args.diarize:
            written = ingest_diarized(
                store,
                segments,
                pyannote_diarize,
                transcriber,
                work_dir=args.out / "work",
                model_name=args.model,
            )
        else:
            written = ingest_transcripts(
                store,
                segments,
                transcriber,
                work_dir=args.out / "work",
                model_name=args.model,
            )
    finally:
        store.close()
    print(f"transcribed {len(segments)} segments -> {written} transcript rows")
    return 0


def _build_transcriber(
    model: str,
    *,
    words: bool,
    store: Store | None = None,
) -> Transcriber:
    """An mlx-whisper transcriber for `model`. `words` requests per-word timings
    (refine needs them; the other passes do not).

    It used to branch to an HF/PEFT loader when `model` named a LoRA adapter
    directory, and took a `base_model` for that path. Training was dropped on
    2026-09-06 (architecture.md, "Scope of the rebuilt product"), so no adapter
    exists to load and the parameter named a choice that can no longer be made.

    With a `store`, each call is biased by the household vocabulary (Whisper's
    initial_prompt, recall.vocabulary) — rebuilt per call, so a term added in the
    UI applies from the very next segment, no restart. The golden gate (score-asr)
    passes no store: it measures the bare model."""

    def mlx(audio: Path) -> AsrResult:
        prompt = build_initial_prompt(store) if store is not None else None
        return mlx_transcribe(audio, model=model, words=words, initial_prompt=prompt)

    return mlx


def _transcriber_for(
    args: argparse.Namespace, *, words: bool, store: Store | None = None
) -> Transcriber:
    """The transcriber for an accuracy pass, from `--model`."""
    return _build_transcriber(args.model, words=words, store=store)


def _result_text(result: AsrResult) -> str:
    """Full text of a transcription (segments joined)."""
    parts = [s.text.strip() for s in result.segments if s.text.strip()]
    return " ".join(parts).strip()


def _cmd_reprocess(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        redone = reprocess(
            store,
            _transcriber_for(args, words=False, store=store),
            work_dir=args.out / "work",
            model_name=args.model,
            max_confidence=args.max_confidence,
        )
    finally:
        store.close()
    print(f"reprocessed {redone} segments with {args.model}")
    return 0


_GOLDEN_FIXTURE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "speech"


@dataclass(frozen=True)
class _GoldenFixture:
    """One clip in the golden ASR gate.

    Each clip is single-language by construction: a mixed-language one trips
    Whisper's one-language-per-segment detection, which is the documented
    code-switching weakness rather than a regression signal. Several clips may
    share a language — two do — and that is coverage, not duplication.

    ⚠ There is no `committed` flag any more. Every clip here ships with the repo
    (2026-09-09), so a missing one is a FAULT rather than a fresh clone's normal
    state — and that is the safer default for whatever is added next: an absent
    fixture fails loudly instead of quietly narrowing what the gate covers, which
    is the whole of #1433.
    """

    audio: str
    reference: str
    language: str
    threshold: float


# ⚠ EVERY fixture here ships with the repo, and #1433 is why that is worth
# stating. The gate advertised a "committed speech fixture" for months while its
# audio existed on ONE Mac, so it could not run on a clone, in CI, or in a nix
# sandbox — a check that read as a repo-wide guarantee and was not one.
#
# The dialogue pair is macOS `say` reading INVENTED lines (plants, a plumber, a
# bakery), rendered by scripts/gen-speech-fixture.sh. It is nobody's voice and
# says nothing about this household, which is what made committing it safe for a
# public repo; the blanket *.flac ignore that had swallowed it was narrowed on
# 2026-09-09. It carries the only Dutch in the gate.
#
# Thresholds are per fixture, and each is set from ITS OWN measured baseline
# rather than copied, because the references differ in exactness and one number
# would silently import the loosest denominator. Measured 2026-09-06 with
# large-v3-turbo, three identical runs each (the decode is deterministic here, so
# headroom is for a runtime/decoder change, not for run-to-run noise):
#   - dialogue-en 0.0123, dialogue-nl 0.0000 — exact references.
#   - public-domain-en 0.0426 — the CANONICAL poem plus the LibriVox preamble,
#     NOT a transcription of this reading, so the reader's own deviations sit in
#     the baseline permanently (see the fixture README).
# Each threshold is baseline + ~0.05, so all three trip on a regression the size
# of the adapter's real-audio one (~0.05 absolute) — equal DETECTION POWER, not
# an equal number. These are DRIFT bounds; none is evidence about absolute
# transcription quality. If a legitimate runtime update trips one, re-baseline it
# deliberately rather than widening it reflexively.
_GOLDEN_FIXTURES = (
    _GoldenFixture(
        audio="public-domain-en.flac",
        reference="public-domain-en.txt",
        language="en",
        threshold=0.09,
    ),
    _GoldenFixture(
        audio="dialogue-en.flac",
        reference="reference-en.txt",
        language="en",
        threshold=0.06,
    ),
    _GoldenFixture(
        audio="dialogue-nl.flac",
        reference="reference-nl.txt",
        language="nl",
        threshold=0.05,
    ),
)


def _cmd_score_asr(args: argparse.Namespace) -> int:
    """Transcribe the golden speech fixtures with the REAL ASR stack and score WER
    against their references — the regression net under the model/decoder seams
    (unit tests stub the ASR). On-demand, not part of verify: it loads the model.

    Scores every fixture, and every fixture ships with the repo — so a clone
    scores exactly what this machine does, Dutch included. A MISSING fixture
    fails the run rather than being skipped: a gate with less to score than it
    claims must not report success, which is the failure mode #1433 recorded.
    """
    transcriber = _build_transcriber(args.model, words=False)
    failed = False
    scored = 0
    for fixture in _GOLDEN_FIXTURES:
        audio = _GOLDEN_FIXTURE / fixture.audio
        if not audio.exists():
            failed = True
            print(f"score-asr: MISSING fixture {fixture.audio}")
            continue
        reference = (_GOLDEN_FIXTURE / fixture.reference).read_text()
        result = transcriber(audio)
        hypothesis = _result_text(result)
        wer = word_error_rate(reference, hypothesis)
        detected = result.language
        lang_note = (
            "" if detected == fixture.language else f" (detected language: {detected}!)"
        )
        print(
            f"score-asr[{fixture.audio}]: WER {wer:.3f} vs threshold "
            f"{fixture.threshold}{lang_note}"
        )
        scored += 1
        if wer > fixture.threshold or detected != fixture.language:
            failed = True
            print(f"--- reference ---\n{reference}")
            print(f"--- hypothesis ---\n{hypothesis}")
    if failed:
        print(
            "score-asr: FAIL - transcription drifted; inspect before trusting "
            "the model/decoder change"
        )
        return 1
    print(f"score-asr: ok (model {args.model}, {scored} fixture(s) scored)")
    return 0


def _cmd_reprobe(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        repaired = reprobe_short_segments(store)
    finally:
        store.close()
    print(f"reprobe: repaired {repaired} truncated-indexed segments")
    return 0


def _cmd_redrive(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        added = redrive_archive(
            store,
            _transcriber_for(args, words=False, store=store),
            silero_speech_regions,
            work_dir=args.out / "work",
            model_name=args.model,
            limit=args.limit,
        )
    finally:
        store.close()
    print(f"redrive: added {added} re-derived transcript rows")
    return 0


def _cmd_score_attribution(args: argparse.Namespace) -> int:
    """Replay a corrected recording through diarize + alignment and report per-word
    speaker-attribution accuracy vs the human-corrected turns, swept over the smoothing
    threshold — so the alignment knobs are tuned on real ground truth, not guessed."""
    if not os.environ.get("HF_TOKEN"):
        print("score-attribution needs HF_TOKEN (pyannote diarization)")
        return 1
    refusal = recording_refusal(args.out, allow=args.while_recording)
    if refusal is not None:
        print(refusal)
        return 1
    from recall.align import assign_words_to_speakers  # noqa: PLC0415 - heavy/gated
    from recall.attribution import (  # noqa: PLC0415 - heavy/gated
        TruthSpan,
        attribution_report,
        score_attribution,
    )

    sweep: list[float] = args.min_turn or [0.3, 0.5, 0.8, 1.2]
    ref = _REF_MIN_TURN if _REF_MIN_TURN in sweep else sweep[len(sweep) // 2]
    store = Store.open(_db_path(args.out))
    try:
        turns = store.session_turns(args.source)
        if not turns:
            print(f"no turns for source {args.source!r}")
            return 1
        work = args.out / "work"
        work.mkdir(parents=True, exist_ok=True)
        run = _AttributionRun(
            chop=args.chop,
            model=args.model,
            work=work,
            clustering_threshold=args.clustering_threshold,
            min_cluster_size=args.min_cluster_size,
        )
        totals: dict[float, list[int]] = {m: [0, 0] for m in sweep}  # m -> [words, ok]
        agg = AttributionReport.empty()  # accumulated breakdown at `ref`
        scored = 0
        # The whole source in time order, so `--context` can reach a segment's
        # neighbours. Ordering is explicit rather than assumed of the query.
        segments = [
            (aid, seg)
            for aid in store.audio_segments_for_source(args.source, limit=100_000)
            if (seg := store.audio_segment(aid)) is not None and Path(seg.path).exists()
        ]
        segments.sort(key=lambda pair: pair[1].start)
        spans = [(s.start.timestamp(), s.end.timestamp()) for _, s in segments]
        durations: dict[str, float] = {}
        for position, (aid, seg) in enumerate(segments):
            if args.max_segments is not None and scored >= args.max_segments:
                print(f"(stopped at --max-segments {args.max_segments})")
                break
            if not (args.while_recording or capture_is_idle(args.out)):
                # The pause elapsed mid-run and the recorder came back on its own.
                # A replay runs for hours, so re-check every segment rather than only
                # at the start — and report what was scored instead of discarding it.
                print("(stopped: capture resumed — the recorder gets the GPU back)")
                break
            truth = [
                TruthSpan(
                    (t.start - seg.start).total_seconds(),
                    (t.end - seg.start).total_seconds(),
                    t.speaker_label,
                )
                for t in turns
                if t.audio_segment_id == aid
                and t.speaker_label
                and not t.speaker_label.startswith("SPEAKER")
            ]
            if not truth:
                continue
            scored += 1
            lo, hi = context_window(position, spans, context=args.context)
            parts = [Path(s.path) for _, s in segments[lo:hi]]
            lengths = _part_durations(parts, durations)
            # Where this segment starts inside the joined window, so predictions come
            # back into segment-relative time and score against the same truth spans
            # the per-segment baseline used.
            centre = sum(lengths[: position - lo])
            pieces = _window_pieces(parts, run, centre=centre, duration=sum(lengths))
            for m in sweep:
                # One aligned prediction list per piece, shifted back into segment time.
                # The cluster label is namespaced by piece: an unchopped replay has one
                # piece and so behaves exactly as before.
                predicted = [
                    ((w.start + w.end) / 2.0 + offset, f"{index}:{run.speaker}")
                    for index, (offset, pwords, pspeakers) in enumerate(pieces)
                    for run in assign_words_to_speakers(pwords, pspeakers, min_turn_s=m)
                    for w in run.words
                ]
                score = score_attribution(predicted, truth)
                totals[m][0] += score.words
                totals[m][1] += score.correct
                if m == ref:
                    agg = agg.merged_with(attribution_report(predicted, truth))
        _print_attribution(args.source, totals, agg, ref)
    finally:
        store.close()
    return 0


@dataclass(frozen=True)
class _AttributionRun:
    """What stays the same for every window of one replay — the model, the scratch dir,
    and whether each segment is cut into independent pieces. Kept together because it
    is genuinely per-run configuration, not per-window input."""

    chop: float | None
    model: str
    work: Path
    clustering_threshold: float | None = None
    min_cluster_size: int | None = None


def _window_pieces(
    parts: list[Path], run: _AttributionRun, *, centre: float, duration: float
) -> list[tuple[float, list[Word], list[SpeakerTurn]]]:
    """Transcribe + diarize the window around one scored segment, with every offset
    shifted back into that segment's own time base (`centre` is where it starts inside
    the window), so predictions score against the same truth spans a `--context 0` run
    used. `duration` comes from the caller's memoised probe — overlapping windows
    revisit the same files, and re-probing here would pay for each of them again.
    """
    from recall.asr import (  # noqa: PLC0415 - heavy/optional path
        make_working_copy,
        scratch_wav,
    )

    with scratch_wav(run.work / f"attr-{parts[0].stem}.wav") as working:
        if len(parts) == 1:
            make_working_copy(parts[0], working)
        else:
            # Normalise each segment on its own and only THEN join, so the centre
            # segment's audio is bit-for-bit what the --context 0 baseline
            # transcribed. Normalising across the join instead makes the run measure
            # gain-sharing as well as window length, which is how the first household
            # comparison went wrong.
            _join_normalised(parts, working, work=run.work)
        pieces = _attribution_pieces(working, run, duration=duration)
    return [(off - centre, pw, ps) for off, pw, ps in pieces]


def _join_normalised(parts: list[Path], dst: Path, *, work: Path) -> None:
    """Join `parts` after normalising each one separately — the join is then the only
    difference from scoring those segments individually."""
    from recall.asr import (  # noqa: PLC0415 - heavy/optional path
        make_working_copy,
        scratch_wav,
    )

    copies: list[Path] = []
    with ExitStack() as stack:
        for index, part in enumerate(parts):
            copy = stack.enter_context(scratch_wav(work / f"join-{index:04d}.wav"))
            make_working_copy(part, copy)
            copies.append(copy)
        concat_working_copy(copies, dst, normalize=False)


def _part_durations(parts: list[Path], cache: dict[str, float]) -> list[float]:
    """Each part's real decoded duration, memoised — overlapping `--context` windows
    revisit the same files, and the recorded duration can trail the file's actual one
    (a row indexed mid-write), which would shift every offset in the window."""
    for part in parts:
        if str(part) not in cache:
            cache[str(part)] = probe_media(part).duration.total_seconds()
    return [cache[str(p)] for p in parts]


def _attribution_pieces(
    working: Path, run: _AttributionRun, *, duration: float
) -> list[tuple[float, list[Word], list[SpeakerTurn]]]:
    """The (offset, words, speakers) units a replay transcribes and diarizes on its own.

    Without `chop` that is the whole segment — the shape an uploaded meeting has in
    production. With it, the segment is cut into `chop`-second pieces and each is
    transcribed and diarized independently: the shape *live capture* has, where a
    conversation arrives as a run of 60 s files, each diarized alone. Cluster labels are
    namespaced per piece because that limitation is the point — a `SPEAKER_00` in one
    file is not the `SPEAKER_00` of the next, and nothing in the pipeline joins them.

    Chopping a recording that scores well is therefore a controlled test of whether the
    diarization *window* is what costs the household its boundary accuracy: same audio,
    same ground truth, only the window changes.
    """
    from recall.asr import scratch_wav, slice_clip  # noqa: PLC0415 - heavy/optional

    def diarize(clip: Path) -> list[SpeakerTurn]:
        return pyannote_diarize(
            clip,
            clustering_threshold=run.clustering_threshold,
            min_cluster_size=run.min_cluster_size,
        )

    if run.chop is None:
        result = mlx_transcribe(working, model=run.model, words=True)
        return [(0.0, list(result.words), list(diarize(working)))]
    pieces: list[tuple[float, list[Word], list[SpeakerTurn]]] = []
    offset = 0.0
    index = 0
    while offset < duration:
        end = min(offset + run.chop, duration)
        if end - offset < 1.0:
            break  # a sub-second tail carries no speech worth diarizing
        with scratch_wav(run.work / f"chop-{index:04d}.wav") as piece:
            slice_clip(working, piece, offset, end)
            result = mlx_transcribe(piece, model=run.model, words=True)
            pieces.append((offset, list(result.words), list(diarize(piece))))
        offset = end
        index += 1
    return pieces


def _print_attribution(
    source: str,
    totals: dict[float, list[int]],
    agg: AttributionReport,
    ref: float,
) -> None:
    """Report the sweep table and the localised error breakdown."""
    print(f"per-word speaker-attribution accuracy for {source!r}:")
    for m, (scored, ok) in totals.items():
        acc = ok / scored if scored else 0.0
        print(f"  min_turn={m:>4}s   {acc:6.1%}   ({ok}/{scored} words)")
    print(f"\nwhere the errors are (min_turn={ref}s):")
    print(
        f"  near a speaker change (<=1s): {agg.near_accuracy:6.1%}"
        f"   ({agg.near_correct}/{agg.near_words})"
    )
    interior = agg.correct - agg.near_correct, agg.words - agg.near_words
    print(
        f"  interior of turns:            {agg.interior_accuracy:6.1%}"
        f"   ({interior[0]}/{interior[1]})"
    )
    print(
        f"  inside short turns (<2s):     {agg.short_accuracy:6.1%}"
        f"   ({agg.short_correct}/{agg.short_words})"
    )
    worst = sorted(agg.errors_by_speaker.items(), key=lambda kv: -kv[1])
    print("  words taken, by true speaker: " + ", ".join(f"{k} {v}" for k, v in worst))


def _cmd_scan_loops(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        hidden = scan_loops(store)
    finally:
        store.close()
    print(f"scan-loops: hid {hidden} repetition-loop turns")
    return 0


def _cmd_scan_foreign_script(args: argparse.Namespace) -> int:
    """Decodes audio, so it is capture-safe only in the sense the other scans are:
    it touches just the candidate segments, not every one."""
    store = Store.open(args.out / "recall.sqlite")
    try:
        hidden = scan_foreign_script(store, silero_speech_regions)
    finally:
        store.close()
    print(f"scan-foreign-script: hid {hidden} non-Latin turns over silence")
    return 0


def _cmd_scan_wordless(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        hidden = scan_empty_text(store)
    finally:
        store.close()
    print(f"scan-wordless: hid {hidden} turns with no word in them")
    return 0


def _cmd_scan_hallucinations(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        result = scan_hallucinations(store, silero_speech_regions)
    finally:
        store.close()
    print(
        f"scanned {result.segments_scanned} audio segments, "
        f"examined {result.turns_examined} turns, "
        f"hid {result.turns_hidden} hallucinations"
    )
    return 0


def _cmd_llm_host(args: argparse.Namespace) -> int:
    """Hold the LLM for everyone who wants it (recall.llmhost). The module is
    imported lazily: it pulls in the web stack, and it is Mac-only."""
    runlog.setup()  # UTC-stamped logging for the LLM host
    from recall.llmhost import serve  # noqa: PLC0415 - keeps the web stack local

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )
    serve(host=args.host, port=args.port, model=args.llm, idle_unload=args.idle_unload)
    return 0


def _cmd_pause(args: argparse.Namespace) -> int:
    """Pause recording on THIS machine directly, with no network — the break-glass
    control for when Isis (the normal pause/resume surface) is unreachable, e.g. mid
    pod-rollout. Writes the same bounded pause file the capture agents self-gate on, so
    it always takes effect on the machine that holds the mic. Isis stays the authority
    when it is reachable: capture-mirror overrides this local state the next time Isis's
    *intent* changes (an unchanged intent is left alone, so a local pause survives)."""
    until = capture_control.pause(args.out, datetime.now(UTC), minutes=args.minutes)
    print(f"paused; recording auto-resumes by {until.isoformat()}")
    return 0


def _cmd_resume(args: argparse.Namespace) -> int:
    """Resume recording on THIS machine directly, with no network (see _cmd_pause for
    how this relates to Isis's intent). A no-op when not paused, so it is safe to run
    blindly to be sure recording is on."""
    capture_control.resume(args.out)
    print("resumed")
    return 0


def _cmd_capture_trace(args: argparse.Namespace) -> int:
    """One readable, time-ordered trace of what capture did: every capture event
    (mirror applications, resume/pause, phone connects/disconnects with their measured
    levels, dead windows) merged with the audio segments actually written. After a
    controlled resume, this says which source recorded what, at what level, and
    when — no guessing."""
    now = datetime.now(UTC)
    since = now - timedelta(minutes=args.minutes)
    store = Store.open(_db_path(args.out))
    try:
        events = store.capture_events_since(since)
        source_ids = [row.id for row in store.source_rows()]
        known = frozenset(path for _, path in store.audio_segment_paths())
        # (utc, tiebreak, line): at the same instant, a segment's start sorts before
        # events noticed at that moment, reading naturally.
        rows: list[tuple[datetime, int, str]] = []
        for event in events:
            who = event.source_id or "household"
            suffix = f"  {event.detail}" if event.detail else ""
            rows.append((event.utc, 1, f"{event.kind:<18} {who}{suffix}"))
        for source_id in source_ids:
            for start, end in store.audio_segment_intervals(source_id, since=since):
                seconds = (end - start).total_seconds()
                rows.append(
                    (
                        start,
                        0,
                        f"{'segment':<18} {source_id}  "
                        f"{seconds:.0f}s (ends {end:%H:%M:%S}Z)",
                    )
                )
            # Segment FILES the worker hasn't indexed yet (min-age guard) — the only
            # place a fresh zero-byte stub is visible before it is dead-windowed, and
            # what makes the trace usable live, mid-sitting, not two minutes later.
            for path in segment_glob(args.out / source_id, source_id):
                if str(path) in known:
                    continue
                try:
                    stat = path.stat()
                    start = parse_segment_start(path.name)
                except (OSError, ValueError):
                    continue
                if datetime.fromtimestamp(stat.st_mtime, tz=UTC) < since:
                    continue
                rows.append(
                    (
                        start,
                        0,
                        f"{'file':<18} {source_id}  "
                        f"{path.name} ({stat.st_size} bytes, not yet indexed)",
                    )
                )
    finally:
        store.close()
    if not rows:
        print(f"no capture events or segments in the last {args.minutes} minutes")
        return 0
    for utc, _tiebreak, line in sorted(rows, key=lambda r: (r[0], r[1])):
        print(f"{utc:%Y-%m-%dT%H:%M:%S}Z  {line}")
    return 0


def _cmd_repair_transcripts(args: argparse.Namespace) -> int:
    """Restore transcripts a refine pass hid and never replaced (see recall.repair).

    Reports by default and changes nothing; `--apply` performs the restore. A hide is
    soft, so this is recovery, not reconstruction: the turns were there all along.
    """
    from recall.repair import (  # noqa: PLC0415
        find_blanked,
        restore,
        retract_into_silence,
    )

    store = Store.open(args.out / "recall.sqlite")
    try:
        blanked = find_blanked(store)
        turns = sum(len(b.restore) for b in blanked)
        for segment in blanked:
            print(
                f"  seg {int(segment.audio_id):<6} "
                f"{len(segment.restore):3d} turns  {segment.preview}"
            )
        print(f"\n{len(blanked)} blanked segment(s), {turns} turn(s) recoverable")

        # The other half: a turn standing on audio the detector heard nothing in is a
        # hallucination, and it blocks the cleanup by making an empty minute look
        # transcribed.
        junk = store.machine_turns_on_silent_audio()
        for _turn_id, text in junk[:10]:
            print(f"  hallucination on silence: {text[:56]!r}")
        print(f"{len(junk)} turn(s) standing on audio the detector heard nothing in")

        if not args.apply:
            print("\n(dry run — pass --apply to restore and retract)")
            return 0
        restored = restore(store, blanked)
        retracted = retract_into_silence(store)
        print(
            f"\nrestored {restored} turn(s); "
            f"retracted {len(retracted)} hallucination(s)"
        )
    finally:
        store.close()
    return 0


_COMMANDS = {
    "pause": _cmd_pause,
    "resume": _cmd_resume,
    "capture-trace": _cmd_capture_trace,
    "repair-transcripts": _cmd_repair_transcripts,
    "transcribe": _cmd_transcribe,
    "reprocess": _cmd_reprocess,
    "score-asr": _cmd_score_asr,
    "reprobe": _cmd_reprobe,
    "redrive": _cmd_redrive,
    "scan-hallucinations": _cmd_scan_hallucinations,
    "scan-loops": _cmd_scan_loops,
    "scan-foreign-script": _cmd_scan_foreign_script,
    "scan-wordless": _cmd_scan_wordless,
    "llm-host": _cmd_llm_host,
    "score-attribution": _cmd_score_attribution,
}


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    # Every entry rotates over-cap logs on start, so rotation doesn't depend on
    # the worker being alive (its loop still rotates during long uptimes). Cheap:
    # only acts on logs over the cap.
    rotate_logs(_LOG_DIR)
    # ⚠ ONE check, at the only place every subcommand passes through, rather than
    # 24 guards at the 24 `mkdir(parents=True)` call sites downstream. A command
    # whose archive is not mounted cannot do anything useful, and the alternative
    # is what live.py did: build the path with parents and ask macOS to create
    # the MOUNTPOINT (562 crashes; see `recall.paths.require_archive` for why the
    # crash was the LUCKY outcome).
    out = getattr(args, "out", None)
    if out is not None:
        try:
            require_archive(out)
        except ArchiveAway as err:
            print(err, file=sys.stderr)
            return 1
    return _COMMANDS[args.command](args)
