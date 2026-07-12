"""Command-line entry point: `python -m recall record|verify|index|search`.

`record` captures a source to segment files (needs the mic + macOS permission).
`verify` scans captured segments and reports gaps/overlaps (Phase 0 check).
`index` ingests captured segments' metadata into the store.
`search` full-text-searches transcripts in the store.
"""

from __future__ import annotations

import argparse
import os
import time
from collections.abc import Callable
from datetime import UTC, date, datetime, timedelta
from pathlib import Path

from recall import capture_control, runlog
from recall.abcompare import Report, compare_models, render_json, render_markdown
from recall.asr import AsrResult, Transcriber, mlx_transcribe
from recall.attribution import AttributionReport
from recall.backup import run_backup
from recall.capture import CaptureConfig
from recall.cleanup import scan_hallucinations, scan_loops
from recall.cli_parser import build_parser
from recall.conversations import segment_conversations
from recall.diarize import pyannote_diarize
from recall.finetune import (
    FinetuneConfig,
    finetune_lora,
    transcribe_clips,
)
from recall.finetune_pilot import PilotReport, run_pilot
from recall.hf_asr import is_adapter_dir, make_hf_transcriber
from recall.identify import identify_segments
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.live import run_live
from recall.llm import Generator, make_mlx_generator
from recall.logrotate import rotate_logs
from recall.loudness import backfill_loudness
from recall.maintenance import (
    backup_age_hours,
    compress_to_opus,
    reprobe_short_segments,
)
from recall.moments import cluster_moments
from recall.probe import scan_segments
from recall.redrive import redrive_archive
from recall.refine import refine_diarized
from recall.reprocess import reprocess
from recall.review import apply_correction
from recall.runner import record
from recall.sources import AudioSource, SourceKind
from recall.speakerid import pyannote_embed
from recall.store import AbCompareJob, Store
from recall.stream_server import serve as serve_ingest
from recall.summarize import days_needing_summaries, summarize_day
from recall.timeline import find_gaps, find_overlaps
from recall.training import export_corpus
from recall.transcript_view import (
    attribution,
    format_conversations,
    format_sessions,
    format_transcript,
    format_turn_details,
)
from recall.vad import silero_speech_regions
from recall.vocabulary import build_initial_prompt
from recall.wer import word_error_rate
from recall.wordtimings import backfill_word_timings
from recall.worker import process_all, process_pending, reconcile_live

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
_LOG_DIR = Path(__file__).resolve().parents[2] / "logs"


def _speaker_id_pass(store: Store, out: Path) -> None:
    """The offline speaker-ID work (no-op without a token; lazy/gated imports):
    prune voiceprints that no longer match a current label, enrol every current
    human-labelled turn into voiceprints (text corrections *and* session-view assigns),
    embed any un-embedded machine turns (once), then cheaply re-derive every turn's
    guess from its stored embedding against the current voiceprints — so guesses stay
    fresh as labelling grows the profiles, with no re-embedding.
    """
    if not os.environ.get("HF_TOKEN"):
        return
    from recall.identify import (  # noqa: PLC0415 - heavy/gated
        backfill_embeddings,
        backfill_voiceprints,
        rematch_speaker_guesses,
    )
    from recall.speakerid import pyannote_embed  # noqa: PLC0415 - heavy/gated

    # Drop prints whose label changed/turn vanished (and legacy correction-sourced
    # rows) so enrolment re-derives them from the current turns.
    pruned = store.prune_stale_voiceprints()
    enrolled = backfill_voiceprints(
        store,
        pyannote_embed,
        work_dir=out / "work",
        now=datetime.now(UTC),
        limit=_VOICEPRINT_BACKFILL_PER_PASS,
    )
    embedded = backfill_embeddings(
        store,
        pyannote_embed,
        work_dir=out / "work",
        limit=_EMBED_BACKFILL_PER_PASS,
    )
    # Cheap re-match only when the landscape changed (prints pruned/enrolled, or new
    # embeddings); idle otherwise.
    if pruned or enrolled or embedded:
        rematch_speaker_guesses(store)


def _db_path(root: Path) -> Path:
    return root / "recall.sqlite"


def _serve_paused_aware(
    out: Path, run_once: Callable[[Callable[[], bool]], int]
) -> int:
    """Run a self-gating recording entrypoint: park while paused, run while active,
    and re-park when a pause interrupts it. `run_once(should_stop)` runs the
    recording until `should_stop()` (a pause) fires or the producer ends; we exit
    (letting KeepAlive respawn) only when it ends for a non-pause reason."""

    def now() -> datetime:
        return datetime.now(UTC)

    def paused() -> bool:
        return capture_control.is_paused(out, now())

    while True:
        capture_control.wait_until_unpaused(out, now=now, sleep=time.sleep)
        result = run_once(paused)
        if not paused():
            return result


def _cmd_record(args: argparse.Namespace) -> int:
    runlog.setup()  # timestamped capture-lifecycle logging to the agent's .err.log
    source = AudioSource(
        id=args.id, name=args.id, kind=SourceKind.COREAUDIO, spec=args.device
    )
    config = CaptureConfig(
        sample_rate=args.sample_rate,
        channels=args.channels,
        segment_seconds=args.segment_seconds,
        codec=args.codec,
        bitrate=args.bitrate,
    )

    # Authoritative registration: the recording agent knows this is the local mic
    # (coreaudio), correcting any kind a worker may have guessed from the directory.
    # Phones self-register as tcp_pcm via the ingest handshake (recall.stream_server).
    store = Store.open(args.out / "recall.sqlite")
    try:
        store.register_source(source)
    finally:
        store.close()

    def once(should_stop: Callable[[], bool]) -> int:
        return record(
            source, config, args.out, max_seconds=args.seconds, should_stop=should_stop
        )

    return _serve_paused_aware(args.out, once)


def _cmd_verify(args: argparse.Namespace) -> int:
    source_dir = args.out / args.id
    segments = scan_segments(source_dir, args.id)
    tolerance = timedelta(milliseconds=args.tolerance_ms)
    gaps = find_gaps(segments, tolerance=tolerance)
    overlaps = find_overlaps(segments, tolerance=tolerance)

    print(f"source {args.id!r}: {len(segments)} segments")
    if segments:
        print(f"  span: {segments[0].start} .. {segments[-1].end}")
    print(f"  overlaps: {len(overlaps)}")
    if gaps:
        print(f"  GAPS: {len(gaps)}")
        for gap in gaps:
            print(f"    {gap.start} .. {gap.end}  ({gap.duration})")
        return 1
    print("  gaps: 0 — continuous coverage ✓")
    return 0


def _cmd_index(args: argparse.Namespace) -> int:
    source = AudioSource(id=args.id, name=args.id, kind=SourceKind.COREAUDIO, spec="")
    segments = scan_segments(args.out / args.id, args.id)
    store = Store.open(_db_path(args.out))
    try:
        store.add_source(source)
        for segment in segments:
            store.add_audio_segment(segment)
    finally:
        store.close()
    print(f"indexed {len(segments)} audio segments for source {args.id!r}")
    return 0


def _cmd_transcribe(args: argparse.Namespace) -> int:
    source = AudioSource(id=args.id, name=args.id, kind=SourceKind.COREAUDIO, spec="")
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


def _cmd_worker(args: argparse.Namespace) -> int:
    def transcriber(audio: Path) -> AsrResult:
        # A short-lived connection just to read the vocabulary (the pass's own
        # store is per-pass and single-thread); rebuilt per segment so new terms
        # apply immediately. connect() skips migrate — the pass migrated already.
        vocab_store = Store.connect(_db_path(args.out))
        try:
            prompt = build_initial_prompt(vocab_store)
        finally:
            vocab_store.close()
        return mlx_transcribe(audio, model=args.model, initial_prompt=prompt)

    # Auto-upgrade to diarized (per-turn language + speakers) when a token is set.
    use_diarize = not args.basic and bool(os.environ.get("HF_TOKEN"))
    diarizer = pyannote_diarize if use_diarize else None
    # VAD gates the basic (no-diarizer) path so silence isn't transcribed.
    vad = None if diarizer else silero_speech_regions
    mode = "diarized" if diarizer else "basic+vad"

    def one_pass() -> int:
        # Safety net: if capture was paused and the pause has elapsed, resume it
        # so recording can never be left off (completeness is the #1 requirement).
        capture_control.auto_resume_if_expired(args.out, datetime.now(UTC))
        store = Store.open(args.out / "recall.sqlite")
        try:
            if args.id is None:
                written = process_all(
                    store,
                    args.out,
                    transcriber,
                    model_name=args.model,
                    diarizer=diarizer,
                    vad=vad,
                    min_age_seconds=args.min_age,
                )
            else:
                source = AudioSource(
                    id=args.id, name=args.id, kind=SourceKind.COREAUDIO, spec=""
                )
                written = process_pending(
                    store,
                    args.out,
                    source,
                    transcriber,
                    model_name=args.model,
                    diarizer=diarizer,
                    vad=vad,
                    min_age_seconds=args.min_age,
                )
            reconcile_live(store)  # drop live transcripts the archive caught up to
            # Cache loudness for new turns off the request path (bounded so the
            # decode loop never competes with capture for long). The labeling
            # queue ranks by this; until it's filled a turn just sorts last.
            backfill_loudness(store, limit=_LOUDNESS_BACKFILL_PER_PASS)
            # Align human-corrected turns to ASR for word timings, so splitting/tight
            # playback on a correction is audio-exact too (not char-interpolated).
            backfill_word_timings(
                store,
                lambda audio: mlx_transcribe(audio, model=args.model, words=True),
                work_dir=args.out / "work",
                limit=_WORD_TIMINGS_BACKFILL_PER_PASS,
            )
            # Offline speaker ID: enrol voiceprints from labels, embed turns once,
            # re-match guesses against current voiceprints (bounded, token-gated).
            _speaker_id_pass(store, args.out)
            return written
        finally:
            store.close()

    if args.loop:
        while True:
            rotate_logs(_LOG_DIR)  # bound the agents' logs; cheap, only acts over-cap
            written = one_pass()
            if written:
                print(f"worker ({mode}): {written} new transcript rows", flush=True)
            time.sleep(args.interval)

    written = one_pass()
    print(f"worker ({mode}): {written} new transcript rows")
    return 0


def _build_transcriber(
    model: str,
    base_model: str,
    *,
    words: bool,
    store: Store | None = None,
) -> Transcriber:
    """A transcriber for `model`: HF/PEFT when it's a LoRA adapter dir (loaded once),
    else mlx-whisper. The adapter is the whole point of "deploy a winning fine-tune" —
    it can't ride the live turbo path, so it lands on these idle-gated passes. `words`
    requests per-word timings (refine needs them; the other passes don't).

    With a `store`, each call is biased by the household vocabulary (Whisper's
    initial_prompt, recall.vocabulary) — rebuilt per call, so a term added in the
    UI applies from the very next segment, no restart. The golden gate (score-asr)
    and A/B runs pass no store: they measure the bare model. (The HF/adapter path
    doesn't support prompt biasing yet — the adapter is un-deployed.)"""
    if is_adapter_dir(model):
        return make_hf_transcriber(model, base_model=base_model, words=words)

    def mlx(audio: Path) -> AsrResult:
        prompt = build_initial_prompt(store) if store is not None else None
        return mlx_transcribe(audio, model=model, words=words, initial_prompt=prompt)

    return mlx


def _transcriber_for(
    args: argparse.Namespace, *, words: bool, store: Store | None = None
) -> Transcriber:
    """The transcriber for an accuracy pass, from `--model`/`--base-model`."""
    return _build_transcriber(args.model, args.base_model, words=words, store=store)


def _result_text(result: AsrResult) -> str:
    """Full text of a transcription (segments joined)."""
    parts = [s.text.strip() for s in result.segments if s.text.strip()]
    return " ".join(parts).strip()


def _run_ab_compare(  # noqa: PLR0913 - selection window + the two models + naming
    store: Store,
    *,
    source: str,
    frm: datetime | None,
    to: datetime | None,
    model_a: str,
    model_b: str,
    base_model: str,
    work_dir: Path,
    limit: int = 100_000,
) -> Report:
    """Build both real transcribers, pick the audio (whole source or [frm, to)), and
    run the non-destructive comparison. Shared by the CLI and the queued-job runner.
    Raises ValueError if the window is half-given or selects no audio."""
    if frm is not None or to is not None:
        if frm is None or to is None:
            raise ValueError("pass from and to together")
        audio_ids = store.audio_segments_in_range(source, frm, to, limit=limit)
    else:
        audio_ids = store.audio_segments_for_source(source, limit=limit)
    if not audio_ids:
        raise ValueError(f"no audio for source {source!r} in that range")
    tr_a = _build_transcriber(model_a, base_model, words=False)
    tr_b = _build_transcriber(model_b, base_model, words=False)
    return compare_models(
        store,
        lambda p: _result_text(tr_a(p)),
        lambda p: _result_text(tr_b(p)),
        audio_ids=audio_ids,
        work_dir=work_dir,
        model_a=model_a,
        model_b=model_b,
    )


def _process_ab_compare_job(
    store: Store, job: AbCompareJob, runner: Callable[[AbCompareJob], Report]
) -> None:
    """Run one queued A/B job via `runner` and persist the outcome: mark it running,
    then save the report (with denormalized summary) or record the error. The runner
    is injected so the persistence logic is unit-testable without any model."""
    store.mark_ab_compare_running(job.id)
    try:
        report = runner(job)
    except Exception as exc:  # any failure is recorded, never crashes the daemon
        store.mark_ab_compare_error(job.id, str(exc))
        print(f"ab-compare: run #{job.id} failed: {exc}")
        return
    store.save_ab_compare_result(
        job.id,
        result_json=render_json(report),
        mean_wer_a=report.mean_wer_a,
        mean_wer_b=report.mean_wer_b,
        n_corrections=len(report.correction_scores),
        n_segments=report.n_segments,
        n_changed=report.n_changed,
    )
    print(
        f"ab-compare: run #{job.id} done — A {report.mean_wer_a} B {report.mean_wer_b}"
    )


def _drain_ab_compare(store: Store, *, work_dir: Path) -> bool:
    """Run one queued A/B comparison if any is pending; return whether it did. Each job
    carries its own models, so the runner builds them per job. Pause-independent — the
    refine daemon calls this every loop regardless of capture state."""
    pending = store.pending_ab_compare_runs(limit=1)
    if not pending:
        return False
    job = pending[0]
    _process_ab_compare_job(
        store,
        job,
        lambda j: _run_ab_compare(
            store,
            source=j.source,
            frm=j.start,
            to=j.end,
            model_a=j.model_a,
            model_b=j.model_b,
            base_model=j.base_model,
            work_dir=work_dir,
        ),
    )
    return True


def _cmd_ab_compare(args: argparse.Namespace) -> int:
    """Run two models over a past recording (or a window of it) and report a
    per-segment text diff + WER against your corrections — without touching the
    store. See recall.abcompare."""
    store = Store.open(args.out / "recall.sqlite")
    try:
        report = _run_ab_compare(
            store,
            source=args.source,
            frm=datetime.fromisoformat(args.frm) if args.frm else None,
            to=datetime.fromisoformat(args.to) if args.to else None,
            model_a=args.model_a,
            model_b=args.model_b,
            base_model=args.base_model,
            work_dir=args.out / "work",
            limit=args.limit,
        )
    except ValueError as exc:
        print(f"ab-compare: {exc}")
        return 1
    finally:
        store.close()
    md = render_markdown(report)
    report_path = args.report or (args.out / f"ab-compare-{args.source}.md")
    report_path.write_text(md)
    report_path.with_suffix(".json").write_text(render_json(report))
    print(md)
    print(f"\n(written to {report_path} and {report_path.with_suffix('.json')})")
    return 0


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


def _cmd_live(args: argparse.Namespace) -> int:
    def once(should_stop: Callable[[], bool]) -> int:
        run_live(
            args.out / "recall.sqlite",
            work_dir=args.out / "work",
            model=args.model,
            device=args.device,
            should_stop=should_stop,
        )
        return 0

    return _serve_paused_aware(args.out, once)


def _cmd_compress(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        count, reclaimed = compress_to_opus(store, bitrate=args.bitrate)
    finally:
        store.close()
    print(f"compressed {count} segments, reclaimed {reclaimed // 1024 // 1024} MB")
    return 0


# Golden ASR gate. One fixture per household language (a mixed-language segment
# trips Whisper's one-language-per-segment detection — the documented
# code-switching weakness, not a regression signal). Measured baselines with
# large-v3-turbo on the committed fixtures (2026-07-02): en 0.012, nl 0.000 —
# 0.15 carries honest headroom for decoder/runtime updates while still catching
# a real regression (the adapter's real-audio regression was ~0.05 absolute).
_GOLDEN_WER_THRESHOLD = 0.15
_GOLDEN_FIXTURE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "speech"
_GOLDEN_LANGUAGES = ("en", "nl")


def _cmd_score_asr(args: argparse.Namespace) -> int:
    """Transcribe the committed speech fixtures with the REAL ASR stack and score
    WER against their references — the regression net under the model/decoder
    seams (unit tests stub the ASR). On-demand, not part of verify: it loads the
    model.
    """
    transcriber = _build_transcriber(args.model, args.base_model, words=False)
    failed = False
    for lang in _GOLDEN_LANGUAGES:
        reference = (_GOLDEN_FIXTURE / f"reference-{lang}.txt").read_text()
        result = transcriber(_GOLDEN_FIXTURE / f"dialogue-{lang}.flac")
        hypothesis = _result_text(result)
        wer = word_error_rate(reference, hypothesis)
        detected = result.language
        lang_note = "" if detected == lang else f" (detected language: {detected}!)"
        print(
            f"score-asr[{lang}]: WER {wer:.3f} vs threshold "
            f"{_GOLDEN_WER_THRESHOLD}{lang_note}"
        )
        if wer > _GOLDEN_WER_THRESHOLD or detected != lang:
            failed = True
            print(f"--- reference ---\n{reference}")
            print(f"--- hypothesis ---\n{hypothesis}")
    if failed:
        print(
            "score-asr: FAIL - transcription drifted; inspect before trusting "
            "the model/decoder change"
        )
        return 1
    print(f"score-asr: ok (model {args.model})")
    return 0


def _drain_day_summaries(
    store: Store, *, llm_model: str, cache: list[Generator]
) -> bool:
    """Summarise ONE missing complete day (the recall layer generates itself in
    the refine daemon — no separate agent). Not idle-gated: a ~20s generation
    doesn't starve capture. The model loads lazily into `cache` so a daemon with
    nothing to summarise never pays for it. Returns whether a day was done."""
    days = days_needing_summaries(store, now=datetime.now(UTC))
    if not days:
        return False
    if not cache:
        cache.append(make_mlx_generator(llm_model))
    summarize_day(store, cache[0], days[0], model_name=llm_model)
    print(f"refine: summarized {days[0]}", flush=True)
    return True


def _cmd_summarize(args: argparse.Namespace) -> int:
    """Generate per-day summaries with the local LLM — one day, or every missing
    complete day. Loads the model once."""
    store = Store.open(_db_path(args.out))
    try:
        days = (
            [args.day]
            if args.day
            else days_needing_summaries(store, now=datetime.now(UTC))
        )
        if not days:
            print("summarize: nothing missing")
            return 0
        generator = make_mlx_generator(args.llm)
        for day in days:
            text = summarize_day(store, generator, day, model_name=args.llm)
            if text is None:
                print(f"summarize: {day} has no visible turns - skipped")
            else:
                print(f"summarize: {day} ({len(text)} chars)")
    finally:
        store.close()
    return 0


def _cmd_reprobe(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        repaired = reprobe_short_segments(store)
    finally:
        store.close()
    print(f"reprobe: repaired {repaired} truncated-indexed segments")
    return 0


def _cmd_search(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        results = store.search(args.query, limit=args.limit)
    finally:
        store.close()
    if not results:
        print(f"no matches for {args.query!r}")
        return 1
    for segment in results:
        lang = f" [{segment.language}]" if segment.language else ""
        src = f" ({segment.source_id})" if segment.source_id else ""
        who = attribution(segment)
        # Stored UTC; shown in the local wall-clock the speech happened on, like the
        # transcript view — so "when" is answerable without converting by hand.
        local = segment.start.astimezone()
        print(f"{local:%Y-%m-%d %H:%M:%S}  {who}{lang}{src}  {segment.text}")
    return 0


def _cmd_show(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        turns = store.turns_by_id(args.ids)
    finally:
        store.close()
    if not turns:
        print(f"no turns found for {args.ids}")
        return 1
    print(format_turn_details(turns))
    return 0


def _cmd_coverage(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        anchor = store.turns_by_id([args.id])
        if not anchor:
            print(f"no turn {args.id}")
            return 1
        turn = anchor[0]
        pad = timedelta(seconds=args.window)
        coverage = store.moment_coverage(turn.start - pad, turn.end + pad)
    finally:
        store.close()
    when = turn.start.astimezone()
    print(f"moment of #{turn.id}  {when:%a %d %b %Y %H:%M:%S} (±{args.window:g}s):")
    for c in coverage:
        rec = "recorded" if c.recorded else "silent"
        print(f"  {c.source_id:8} {rec:9} turns={c.turns}")
    return 0


def _cmd_correct(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        turns = store.session_turns(args.session)
        if not turns:
            print(f"no current segments for session {args.session!r}")
            return 1
        mode = "APPLY" if args.apply else "DRY-RUN"
        print(f"session {args.session}: {len(turns)} segments  ::  mode = {mode}\n")
        ok = True
        for old, new in args.fix:
            matches = [t for t in turns if old in t.text]
            if len(matches) != 1:
                print(
                    f"!! {old!r} matched {len(matches)} segment(s) "
                    f"(need exactly 1) -- SKIP\n"
                )
                ok = False
                continue
            seg = matches[0]
            corrected = seg.text.replace(old, new)
            print(f"#{seg.id}  [{seg.speaker_label}]")
            print(f"   OLD: {seg.text}")
            print(f"   NEW: {corrected}")
            if args.apply:
                new_id = apply_correction(
                    store, seg.id, corrected, now=datetime.now(UTC)
                )
                print(f"   -> applied as new segment #{new_id}")
            print()
    finally:
        store.close()
    if not args.apply:
        print("DRY-RUN only -- nothing written. Re-run with --apply to commit.")
    return 0 if ok else 1


def _day_bounds(day: str) -> tuple[datetime, datetime]:
    """[start, end) of a local day in UTC. `day` is 'today' or YYYY-MM-DD."""
    tz = datetime.now().astimezone().tzinfo
    d = datetime.now(tz).date() if day == "today" else date.fromisoformat(day)
    start = datetime(d.year, d.month, d.day, tzinfo=tz)
    return start.astimezone(UTC), (start + timedelta(days=1)).astimezone(UTC)


def _cmd_transcript(args: argparse.Namespace) -> int:
    store = Store.open(_db_path(args.out))
    try:
        if args.day:
            return _transcript_day(store, args)
        if not args.session:
            print(format_sessions(store.session_summaries(), as_json=args.json))
            return 0
        turns = store.session_turns(args.session)
        if not turns:
            print(f"no transcript for session {args.session!r}")
            return 1
        print(format_transcript(args.session, turns, as_json=args.json))
    finally:
        store.close()
    return 0


def _transcript_day(store: Store, args: argparse.Namespace) -> int:
    """A day's continuous-capture conversations (split by silence): list them, or with
    --conv N dump one. Redundant mics are folded to one primary turn per moment."""
    start, end = _day_bounds(args.day)
    turns = sorted(
        store.recent_transcripts(limit=10000, before=end, after=start),
        key=lambda t: t.start,
    )
    convs = segment_conversations(turns)
    if args.conv is not None:
        if args.conv == "last":
            n = len(convs)
        elif args.conv.lstrip("-").isdigit():
            n = int(args.conv)
        else:
            print(f"--conv must be a number or 'last', not {args.conv!r}")
            return 1
        if not 1 <= n <= len(convs):
            print(f"no conversation {args.conv} on {args.day} (have {len(convs)})")
            return 1
        moments = cluster_moments(convs[n - 1].turns)
        primary = [t for m in moments for t in m.primary]
        label = f"{args.day} · conversation {n}"
        print(format_transcript(label, primary, as_json=args.json))
        return 0
    rows = []
    for n, conv in enumerate(convs, 1):
        moments = cluster_moments(conv.turns)
        count = sum(len(m.primary) for m in moments)
        first = moments[0].primary if moments else ()
        preview = first[0].text[:60] if first else ""
        rows.append((n, conv.start, conv.end, count, preview))
    print(format_conversations(args.day, rows, as_json=args.json))
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


def _cmd_ingest(args: argparse.Namespace) -> int:
    runlog.setup()  # timestamped connect/disconnect logging to the agent's .err.log
    serve_ingest(args.out, args.port)
    return 0


def _refine_one_source(
    store: Store, args: argparse.Namespace, *, diarize_enabled: bool
) -> int:
    """Deliberate one-shot re-derive of a single recording — not idle-gated (the
    operator chose to run it), processes every segment, then exits."""
    if not diarize_enabled:
        store.close()
        print("refine --source needs HF_TOKEN (diarization is gated)")
        return 1
    try:
        turns = refine_diarized(
            store,
            pyannote_diarize,
            _transcriber_for(args, words=True, store=store),
            pyannote_embed,
            work_dir=args.out / "work",
            model_name=args.model,
            source=args.source,
        )
    finally:
        store.close()
    print(f"refine: re-derived source {args.source!r}, {turns} turn(s)")
    return 0


def _cmd_refine(args: argparse.Namespace) -> int:
    """Diarize-refine the archive, but only while capture is idle (paused) so the
    heavy pyannote pass never competes with live recording. Runs as a daemon by
    default; --max-segments N does a bounded run (one segment at a time, re-checking
    the pause state before each) and exits.

    The daemon also drains queued A/B model comparisons (`recall ab-compare` from the
    web UI). Those are operator-chosen and read-only, so they run regardless of the
    pause state and need no HF_TOKEN — diarization is what's gated, not comparison."""
    diarize_enabled = bool(os.environ.get("HF_TOKEN"))
    store = Store.open(args.out / "recall.sqlite")

    if args.source:
        return _refine_one_source(store, args, diarize_enabled=diarize_enabled)

    # Built lazily: the diarize passes need it, but an HF-token-less daemon that only
    # services ab-compare jobs must not require the refine adapter at all.
    transcriber = (
        _transcriber_for(args, words=True, store=store) if diarize_enabled else None
    )

    def diarize_one(*, redo: bool) -> int:
        assert transcriber is not None  # only called on the diarize_enabled branches
        return refine_diarized(
            store,
            pyannote_diarize,
            transcriber,
            pyannote_embed,
            work_dir=args.out / "work",
            model_name=args.model,
            limit=1,
            redo=redo,
        )

    def refine_request_one() -> tuple[int, int]:
        """Process one on-demand request from the web — refine exactly its window's
        segments. Returns (turns added, segments processed)."""
        assert transcriber is not None  # only called on the diarize_enabled branches
        req = store.pending_refine_requests(limit=1)[0]
        ids = store.audio_segments_in_range(
            req.source, req.start, req.end, limit=10_000
        )
        added = refine_diarized(
            store,
            pyannote_diarize,
            transcriber,
            pyannote_embed,
            work_dir=args.out / "work",
            model_name=args.model,
            audio_ids=ids,
        )
        store.mark_refine_request_done(req.id)
        print(f"refine: request #{req.id} ({req.source}, {len(ids)} seg) -> {added}")
        return added, len(ids)

    segments = turns = 0
    summarizer: list[Generator] = []  # lazy one-slot cache for the summary drain
    try:
        while args.max_segments == 0 or segments < args.max_segments:
            now = datetime.now(UTC)
            # A/B comparisons run first and regardless of pause — operator-chosen and
            # read-only, so they don't wait for an idle window or need a token.
            if _drain_ab_compare(store, work_dir=args.out / "work"):
                continue
            if _drain_day_summaries(store, llm_model=args.llm, cache=summarizer):
                continue
            idle = diarize_enabled and capture_control.is_paused(args.out, now)
            if idle and store.pending_refine_requests(limit=1):
                added, n = refine_request_one()  # on-demand requests first
                turns += added
                segments += n
            elif idle and store.audio_segments_to_diarize(limit=1):
                turns += diarize_one(redo=False)  # never-diarized audio first
                segments += 1
            elif idle and store.audio_segments_to_rediarize(limit=1):
                turns += diarize_one(redo=True)  # then upgrade older diarized days
                segments += 1
            elif args.max_segments:
                break  # bounded run: nothing to do right now, so stop
            else:
                time.sleep(args.poll_seconds)  # capture active or caught up — idle
    finally:
        store.close()
    print(f"refine: diarized {segments} segment(s), {turns} turn(s)")
    return 0


def _cmd_score_attribution(args: argparse.Namespace) -> int:
    """Replay a corrected recording through diarize + alignment and report per-word
    speaker-attribution accuracy vs the human-corrected turns, swept over the smoothing
    threshold — so the alignment knobs are tuned on real ground truth, not guessed."""
    if not os.environ.get("HF_TOKEN"):
        print("score-attribution needs HF_TOKEN (pyannote diarization)")
        return 1
    from collections import Counter  # noqa: PLC0415

    from recall.align import assign_words_to_speakers  # noqa: PLC0415 - heavy/gated
    from recall.asr import make_working_copy, scratch_wav  # noqa: PLC0415 - heavy/gated
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
        totals: dict[float, list[int]] = {m: [0, 0] for m in sweep}  # m -> [words, ok]
        agg = AttributionReport(0, 0, 0, 0, 0, 0, {})  # accumulated breakdown at `ref`
        errors: Counter[str] = Counter()
        for aid in store.audio_segments_for_source(args.source, limit=100_000):
            seg = store.audio_segment(aid)
            if seg is None or not Path(seg.path).exists():
                continue
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
            with scratch_wav(work / f"attr-{int(aid):06d}.wav") as working:
                make_working_copy(Path(seg.path), working)
                result = mlx_transcribe(working, model=args.model, words=True)
                words = list(result.words)
                speakers = list(pyannote_diarize(working))
            for m in sweep:
                runs = assign_words_to_speakers(words, speakers, min_turn_s=m)
                predicted = [
                    ((w.start + w.end) / 2.0, run.speaker)
                    for run in runs
                    for w in run.words
                ]
                score = score_attribution(predicted, truth)
                totals[m][0] += score.words
                totals[m][1] += score.correct
                if m == ref:
                    rep = attribution_report(predicted, truth)
                    agg = AttributionReport(
                        agg.words + rep.words,
                        agg.correct + rep.correct,
                        agg.near_words + rep.near_words,
                        agg.near_correct + rep.near_correct,
                        agg.short_words + rep.short_words,
                        agg.short_correct + rep.short_correct,
                        {},
                    )
                    errors.update(rep.errors_by_speaker)
        _print_attribution(args.source, totals, agg, dict(errors), ref)
    finally:
        store.close()
    return 0


def _print_attribution(
    source: str,
    totals: dict[float, list[int]],
    agg: AttributionReport,
    errors: dict[str, int],
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
    worst = sorted(errors.items(), key=lambda kv: -kv[1])
    print("  words taken, by true speaker: " + ", ".join(f"{k} {v}" for k, v in worst))


# The mirror runs nightly; 48h of slack tolerates one missed night (Mac asleep,
# odin briefly down) without letting a dead backup go quiet for a week.
_BACKUP_MAX_AGE_HOURS = 48.0


def _cmd_doctor(args: argparse.Namespace) -> int:
    """Check every installed recall agent is loaded. Self-gating means agents stay
    loaded even while paused (they park, they don't unload), so any installed-but-
    missing agent is a real fault. Exits non-zero if any is missing."""
    health = capture_control.agent_health()
    if not health:
        print("doctor: no recall agents installed")
        return 1
    for label, loaded in health:
        print(f"  [{'ok' if loaded else 'MISSING'}] {label}")
    paused = capture_control.is_paused(args.out, datetime.now(UTC))
    print(f"capture: {'paused' if paused else 'recording'}")
    missing = [label for label, loaded in health if not loaded]
    # The archive's only unrecoverable failure mode is losing the one local copy —
    # a silently-dead off-machine mirror must fail the health check loudly.
    age = backup_age_hours(args.out)
    stale = age is None or age > _BACKUP_MAX_AGE_HOURS
    if age is None:
        print("backup: NEVER completed (no .last-backup-ok marker)")
    else:
        print(f"backup: last succeeded {age:.1f}h ago{' (STALE)' if stale else ''}")
    if missing:
        print(f"doctor: {len(missing)} agent(s) NOT loaded: {', '.join(missing)}")
        return 1
    if stale:
        print("doctor: off-machine backup is stale — check logs/backup.err.log")
        return 1
    print("doctor: all agents loaded, backup fresh")
    return 0


def _cmd_scan_loops(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        hidden = scan_loops(store)
    finally:
        store.close()
    print(f"scan-loops: hid {hidden} repetition-loop turns")
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


def _cmd_api(args: argparse.Namespace) -> int:
    # uvicorn is imported lazily so every other CLI command stays free of the
    # web stack. RECALL_OUT is read by recall.api at import time.
    import uvicorn  # noqa: PLC0415 - keep the web stack out of other commands

    runlog.setup()  # timestamped action logging (pause/resume) before uvicorn
    os.environ["RECALL_OUT"] = str(args.out)
    uvicorn.run("recall.api:app", host=args.host, port=args.port, log_level="info")
    return 0


def _cmd_enroll(args: argparse.Namespace) -> int:
    embedding = pyannote_embed(args.audio, model=args.model)
    store = Store.open(args.out / "recall.sqlite")
    try:
        speaker_id = store.enroll_speaker(args.name, embedding, now=datetime.now(UTC))
    finally:
        store.close()
    print(f"enrolled {args.name!r} (speaker {speaker_id})")
    return 0


def _cmd_identify(args: argparse.Namespace) -> int:
    def embedder(audio: Path) -> list[float]:
        return pyannote_embed(audio, model=args.model)

    store = Store.open(args.out / "recall.sqlite")
    try:
        resolved = identify_segments(
            store, embedder, work_dir=args.out / "work", threshold=args.threshold
        )
    finally:
        store.close()
    print(f"resolved {resolved} segments to enrolled speakers")
    return 0


def _cmd_export_training(args: argparse.Namespace) -> int:
    store = Store.open(args.out / "recall.sqlite")
    try:
        count = export_corpus(store, args.dest)
    finally:
        store.close()
    print(f"exported {count} training examples to {args.dest}")
    return 0


def _cmd_finetune(args: argparse.Namespace) -> int:
    adapter = finetune_lora(
        FinetuneConfig(
            manifest=args.manifest,
            output_dir=args.dest,
            base_model=args.base_model,
            epochs=args.epochs,
            learning_rate=args.lr,
            lora_rank=args.lora_rank,
            eval_holdout=args.eval_holdout,
            early_stopping_patience=args.early_stopping_patience,
        )
    )
    print(f"saved LoRA adapter to {adapter}")
    return 0


def _print_pilot_report(report: PilotReport) -> None:
    print()
    print("================= PILOT RESULT =================")
    print(f"  train / held-out: {report.train_count} / {report.test_count} clips")
    print(f"  base    held-out WER: {report.base.wer * 100:5.1f}%")
    print(f"  adapter held-out WER: {report.adapter.wer * 100:5.1f}%")
    verdict = (
        "ADAPTER WINS"
        if report.adapter_wins
        else ("tie" if report.delta == 0 else "base wins")
    )
    print(f"  delta: {report.delta * 100:+.1f} pts  ->  {verdict}")
    print("================================================")
    print()
    print("per-clip (ref | base | adapter):")
    for base_clip, adapt_clip in zip(
        report.base.per_clip, report.adapter.per_clip, strict=True
    ):
        print(f"  ref : {base_clip.ref[:72]}")
        print(f"  base: {base_clip.hyp[:72]}")
        print(f"  adpt: {adapt_clip.hyp[:72]}")
        print()


def _cmd_finetune_pilot(args: argparse.Namespace) -> int:
    dest = args.dest or (args.out / "pilot-finetune")

    def transcribe(records: list[dict[str, object]], adapter: Path | None) -> list[str]:
        return transcribe_clips(
            records, base_model=args.base_model, adapter_dir=adapter
        )

    def train(manifest: Path) -> Path:
        # Tiny batch + accumulation + checkpointing so large-v3 fp32 fits unified
        # memory (a full batch-8 forward OOMs ~42 GB).
        return finetune_lora(
            FinetuneConfig(
                manifest=manifest,
                output_dir=dest / "run",
                base_model=args.base_model,
                epochs=args.epochs,
                lora_rank=args.lora_rank,
                batch_size=1,
                grad_accum=8,
                gradient_checkpointing=True,
            )
        )

    # Pause capture for the duration: with the recorder stopped there is no
    # capture buffer for the heavy run to starve, which is the whole cardinal-rule
    # risk. Resume in finally so a crash can't leave recording off.
    pause = not args.no_pause_capture
    if pause:
        until = capture_control.pause(args.out, datetime.now(UTC))
        print(f"capture paused until {until:%H:%M:%S} for the pilot run")
    try:
        store = Store.open(_db_path(args.out))
        try:
            report = run_pilot(
                store,
                dest,
                transcribe=transcribe,
                train=train,
                holdout=args.holdout,
            )
        finally:
            store.close()
    finally:
        if pause:
            capture_control.resume(args.out)
            print("capture resumed")

    _print_pilot_report(report)
    return 0


def _cmd_sync(args: argparse.Namespace) -> int:
    """Push the local archive to the fleet's system of record (the Isis split). The
    token is read from RECALL_SYNC_TOKEN. Imports are lazy so `recall.cli` stays ML- and
    framework-free for the capture agents (recall.sync drags in the web framework)."""
    token = os.environ.get("RECALL_SYNC_TOKEN")
    if not token:
        print("sync needs RECALL_SYNC_TOKEN")
        return 1
    from recall.sync import SyncClient  # noqa: PLC0415 - lazy: pulls the web framework
    from recall.sync_push import sync_push  # noqa: PLC0415

    store = Store.open(args.out / "recall.sqlite")
    try:
        pushed = sync_push(store, SyncClient(args.url, token))
    finally:
        store.close()
    print(f"sync: pushed {pushed} segment(s) to {args.url}")
    return 0


def _cmd_scan_quiet(args: argparse.Namespace) -> int:
    """Measure each capture segment's raw volume (cached, resumable) and list the long
    total-quiet spans — candidates for the cleanup UI to review and delete."""
    from recall.quiet import quiet_spans, scan_segments  # noqa: PLC0415

    store = Store.open(args.out / "recall.sqlite")
    try:
        measured = 0
        while (n := scan_segments(store)) > 0:
            measured += n
            print(f"scan-quiet: measured {measured} segments...", flush=True)
        spans = quiet_spans(store, min_duration_s=float(args.min_seconds))
        for span in spans:
            print(
                f"  {span.start:%Y-%m-%d %H:%M} .. {span.end:%H:%M}  "
                f"{span.duration_s / 60:5.0f} min  ({len(span.audio_ids)} segments)"
            )
        total_h = sum(s.duration_s for s in spans) / 3600
        print(f"scan-quiet: {len(spans)} quiet span(s), {total_h:.1f}h total")
    finally:
        store.close()
    return 0


def _cmd_backup(args: argparse.Namespace) -> int:
    """Mirror the archive off-machine (see recall.backup). Runs in the recall python
    context so it has the external volume's TCC grant — the reason this is a command
    and not the old shell agent, whose bare rsync was denied the volume after a
    remount reset TCC."""
    run_backup(args.out, args.dest)
    print(f"backup: mirrored {args.out} to {args.dest}")
    return 0


_COMMANDS = {
    "record": _cmd_record,
    "backup": _cmd_backup,
    "sync": _cmd_sync,
    "scan-quiet": _cmd_scan_quiet,
    "verify": _cmd_verify,
    "index": _cmd_index,
    "transcribe": _cmd_transcribe,
    "reprocess": _cmd_reprocess,
    "worker": _cmd_worker,
    "ingest": _cmd_ingest,
    "live": _cmd_live,
    "compress": _cmd_compress,
    "score-asr": _cmd_score_asr,
    "summarize": _cmd_summarize,
    "reprobe": _cmd_reprobe,
    "coverage": _cmd_coverage,
    "search": _cmd_search,
    "show": _cmd_show,
    "transcript": _cmd_transcript,
    "correct": _cmd_correct,
    "redrive": _cmd_redrive,
    "refine": _cmd_refine,
    "doctor": _cmd_doctor,
    "scan-hallucinations": _cmd_scan_hallucinations,
    "scan-loops": _cmd_scan_loops,
    "api": _cmd_api,
    "export-training": _cmd_export_training,
    "finetune": _cmd_finetune,
    "finetune-pilot": _cmd_finetune_pilot,
    "score-attribution": _cmd_score_attribution,
    "ab-compare": _cmd_ab_compare,
    "enroll": _cmd_enroll,
    "identify": _cmd_identify,
}


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    # Every entry rotates over-cap logs on start, so rotation doesn't depend on
    # the worker being alive (its loop still rotates during long uptimes). Cheap:
    # only acts on logs over the cap.
    rotate_logs(_LOG_DIR)
    return _COMMANDS[args.command](args)
