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
import threading
import time
import traceback
from collections.abc import Callable
from contextlib import ExitStack
from dataclasses import dataclass
from datetime import UTC, date, datetime, timedelta
from pathlib import Path

from recall import capture_control, heartbeat, runlog
from recall.asr import (
    AsrResult,
    Transcriber,
    Word,
    concat_working_copy,
    mlx_transcribe,
)
from recall.attribution import AttributionReport, context_window
from recall.beat_relay import serve as serve_beat_relay
from recall.capture import parse_segment_start, segment_glob
from recall.cleanup import (
    scan_empty_text,
    scan_foreign_script,
    scan_hallucinations,
    scan_loops,
)
from recall.cli_parser import build_parser
from recall.conversations import segment_conversations
from recall.diarize import SpeakerTurn, pyannote_diarize
from recall.identify import identify_segments
from recall.ingest import ingest_diarized, ingest_transcripts
from recall.live import run_live
from recall.logrotate import rotate_logs
from recall.loudness import backfill_loudness
from recall.maintenance import (
    reprobe_short_segments,
)
from recall.moments import cluster_moments
from recall.paths import ArchiveAway, require_archive
from recall.probe import probe_media, scan_segments
from recall.redrive import redrive_archive
from recall.refine import refine_diarized
from recall.reprocess import reprocess
from recall.review import apply_correction
from recall.sources import AudioSource, SourceKind
from recall.speakerid import pyannote_embed
from recall.store import Store
from recall.timeline import find_gaps, find_overlaps
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
# An absolute path rather than one derived from __file__: the agents run this source
# from a read-only store copy (deploy/hm-agents.nix), where "next to the code" is both
# the wrong place and unwritable — rotation would quietly stop. RECALL_LOG_DIR
# overrides it.
_LOG_DIR = Path(os.environ.get("RECALL_LOG_DIR") or Path.home() / "Library/Logs/recall")


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


def _serve_paused_aware(
    out: Path,
    run_once: Callable[[Callable[[], bool]], int],
    *,
    record_event: Callable[[capture_control.CaptureEventKind, datetime], None]
    | None = None,
) -> int:
    """Run a self-gating recording entrypoint: park while paused, run while active,
    and re-park when a pause interrupts it. `run_once(should_stop)` runs the
    recording until `should_stop()` (a pause) fires or the producer ends; we exit
    (letting KeepAlive respawn) only when it ends for a non-pause reason.

    `record_event(kind, utc)` (optional) durably marks the resume/pause transitions —
    the ground truth of when capture was actually running, which a loss check reconciles
    against timeline gaps. It runs on a daemon thread and swallows errors, so this
    bookkeeping can never stall or crash the recorder (completeness beats an audit)."""

    def now() -> datetime:
        return datetime.now(UTC)

    def paused() -> bool:
        return capture_control.is_paused(out, now())

    def note(kind: capture_control.CaptureEventKind) -> None:
        if record_event is None:
            return
        stamped = now()

        def write() -> None:
            try:
                record_event(kind, stamped)
            except Exception:
                logging.getLogger("recall.capture").warning(
                    "capture-event %r not recorded", kind, exc_info=True
                )

        threading.Thread(target=write, daemon=True).start()

    while True:
        capture_control.wait_until_unpaused(out, now=now, sleep=time.sleep)
        note(capture_control.CaptureEventKind.RESUME)  # capture is becoming active
        result = run_once(paused)
        if not paused():
            return result
        note(
            capture_control.CaptureEventKind.PAUSE
        )  # a pause stopped it (not an EOF exit)


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
    source = _source_found_on_disk(args.id)
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


def _cmd_worker(args: argparse.Namespace) -> int:
    runlog.setup()  # UTC-stamped logging for the archive pass

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
        # ⚠ Stamp the START, not just the end. A pass that never returns and a loop
        # that never starts one look identical from a finish-only heartbeat, and
        # they point at different things: the first is the archive, the second is
        # launchd. This is the only proof that a pass happened at all when it found
        # nothing to do — the worker's log says nothing in that case, which is how
        # an hour of unindexed audio stayed invisible on 2026-08-10 (#709).
        started_at = datetime.now(UTC)
        started_clock = time.monotonic()
        heartbeat.write(
            args.out, heartbeat.Beat(started_at, finished=None, seconds=None, rows=0)
        )
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
                source = _source_found_on_disk(args.id)
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
            # Hide the filler this pass just created. Gated on `written` because new
            # turns are the only way new junk appears, and scheduled HERE because
            # the lesson of scan-loops and scan-hallucinations is that a cleanup
            # nobody runs cleans nothing: both have existed for months as hand-only
            # commands while 214 wordless turns accumulated in the read path.
            # Cheap in steady state — the wordless check is pure text, and the
            # foreign-script one decodes audio only for candidates, of which a
            # swept archive has almost none.
            if written:
                scan_empty_text(store)
                scan_foreign_script(store, silero_speech_regions)
        finally:
            store.close()
        # Only on the way out clean: a pass that raised did not complete, and a
        # crash-looping worker must keep reading as one whose passes never return
        # rather than as one ticking over nicely.
        heartbeat.write(
            args.out,
            heartbeat.Beat(
                started_at,
                finished=datetime.now(UTC),
                seconds=time.monotonic() - started_clock,
                rows=written,
            ),
        )
        return written

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


def _live_sync_loop(
    out: Path, url: str, token: str, interval: float, stop: threading.Event
) -> None:
    """Background push of new live turns to the fleet's instant feed, every `interval`s.

    Its own store connection (sqlite is single-thread) and fully off the VAD loop, so a
    slow or unreachable fleet never touches capture or transcription. Best-effort: a
    failed push is logged and retried next tick — the archive segment push carries the
    turns regardless, so a dropped live push only delays the instant feed, never loses.
    """
    from recall.sync import SyncClient  # noqa: PLC0415 - lazy: pulls the web framework
    from recall.sync_push import push_live_turns  # noqa: PLC0415

    log = logging.getLogger("recall.live")
    # ⚠ SETUP IS INSIDE THE GUARD, not before it. Opening the store touches
    # /Volumes/Backup, which intermittently stops answering (#1412) — and this
    # runs on a DAEMON thread, so an exception here used to escape as a bare
    # traceback, kill the thread, and leave the live agent running happily with
    # its instant feed silently off until the next restart. The loop below was
    # already guarded ("a push must never crash the live agent"); the two lines
    # that reach the disk were the ones outside it.
    try:
        client = SyncClient(url, token)
        store = Store.open(out / "recall.sqlite")
    except Exception:
        log.exception("live-sync: could not start — instant feed is OFF until restart")
        return
    try:
        while not stop.wait(interval):
            try:
                push_live_turns(store, client)
            except Exception:  # best-effort; a push must never crash the live agent
                log.warning("live-sync: push failed (will retry)", exc_info=True)
    finally:
        store.close()


def _cmd_live(args: argparse.Namespace) -> int:
    # ⚠ TIMESTAMPS, without which this agent's failures cannot be diagnosed at all.
    # Measured 2026-09-09: live is silent while the microphone it reads hears speech
    # for a quarter of all recording time (#1383), and `live.err.log` holds 562
    # `PermissionError: /Volumes/Backup`, 562 `FileNotFoundError` and 417
    # `httpx.ConnectError` — none of which can be lined up against a single stall,
    # because the file carries raw stderr and no clock. Every other long-running
    # agent already calls this; live was the one that did not, which is precisely
    # why its stalls stayed inferable rather than measurable.
    runlog.setup()

    # Push the instant feed to the fleet on its own thread when the split is configured
    # (a fleet URL + token); LAN-only deployments leave --fleet-url empty and are
    # untouched. The thread never shares the VAD loop's store or timing.
    stop = threading.Event()
    sync_thread: threading.Thread | None = None
    token = os.environ.get("RECALL_SYNC_TOKEN")
    if args.fleet_url and token:
        sync_thread = threading.Thread(
            target=_live_sync_loop,
            args=(args.out, args.fleet_url, token, args.live_interval, stop),
            daemon=True,
        )
        sync_thread.start()

    def once(should_stop: Callable[[], bool]) -> int:
        run_live(
            args.out / "recall.sqlite",
            work_dir=args.out / "work",
            model=args.model,
            device=args.device,
            should_stop=should_stop,
        )
        return 0

    try:
        return _serve_paused_aware(args.out, once)
    finally:
        stop.set()
        if sync_thread is not None:
            sync_thread.join(timeout=5)


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


def _cmd_beat_relay(args: argparse.Namespace) -> int:
    runlog.setup()
    serve_beat_relay(args.port, args.fleet_url)
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
    runlog.setup()  # UTC-stamped logging for the refine pass
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
    try:
        while args.max_segments == 0 or segments < args.max_segments:
            now = datetime.now(UTC)
            # Start each pass on a clean connection. A write that failed under lock
            # contention leaves an aborted transaction open, which freezes this
            # connection's read snapshot — the daemon then never sees an ask the jobs
            # runner queued and sleeps forever with it pending (the bug that silently
            # hung Ask). Rolling back is a no-op when nothing is open.
            store.rollback()
            try:
                # Ask, A/B comparison and day-summaries used to be drained here.
                # All three were cut with the product's scope (architecture.md);
                # what is left is the diarizing refine pass this daemon is named
                # for, which stays idle-gated so it never competes with capture.
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
            except Exception:  # one bad pass must never wedge the daemon
                # Recover the connection and keep serving — the failed unit is left for
                # the next pass to retry (or time out on the fleet).
                store.rollback()
                traceback.print_exc()
                time.sleep(args.poll_seconds)
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


def _cmd_sync(args: argparse.Namespace) -> int:
    """Push the local archive to the fleet's system of record (the Isis split). The
    token is read from RECALL_SYNC_TOKEN. Imports are lazy so `recall.cli` stays ML- and
    framework-free for the capture agents (recall.sync drags in the web framework)."""
    runlog.setup()  # UTC-stamped logging for the fleet sync pass
    token = os.environ.get("RECALL_SYNC_TOKEN")
    if not token:
        print("sync needs RECALL_SYNC_TOKEN")
        return 1
    from recall.sync import SyncClient  # noqa: PLC0415 - lazy: pulls the web framework
    from recall.sync_push import pull_labels, sync_push  # noqa: PLC0415

    store = Store.open(args.out / "recall.sqlite")
    client = SyncClient(args.url, token)
    started = time.monotonic()
    try:
        pushed = sync_push(store, client)
        # Reverse leg: bring the fleet's human voice-namings home. The UI is on the
        # fleet, so this is the only path a name reaches the master archive and the
        # voiceprint enrolment.
        named = pull_labels(store, client)
    finally:
        store.close()
    # ⚠ LOGGED, not printed, and with its DURATION — both were missing and both
    # were load-bearing. `sync.out.log` carried no clock at all, so a pass could
    # be counted but never timed: #1346 asks for a before/after on a real deep
    # catch-up (passes of 36-81 and 500 segments are both in the record) and the
    # measurement was unrecoverable from 33,501 lines of undated output.
    logging.getLogger("recall.sync").info(
        "sync: pushed %d segment(s), pulled %d voice-naming(s) in %.1fs — %s",
        pushed,
        named,
        time.monotonic() - started,
        args.url,
    )
    return 0


def _cmd_jobs(args: argparse.Namespace) -> int:
    """Run on-demand work the fleet requested but can't do itself (the Isis split):
    pull its refine queue into this Mac's local queue (the idle refine daemon drains
    it), and fetch uploaded sessions into this Mac's archive (the worker transcribes
    them; results sync back on their own). The Mac holds the ML and mic; the fleet
    holds only the UI. Token is RECALL_SYNC_TOKEN. Imports are lazy so recall.cli stays
    framework-free for the capture agents (recall.sync pulls the web framework)."""
    runlog.setup()  # UTC-stamped logging for the job runner
    token = os.environ.get("RECALL_SYNC_TOKEN")
    if not token:
        print("jobs needs RECALL_SYNC_TOKEN")
        return 1
    from recall.jobs import run_jobs_once  # noqa: PLC0415
    from recall.sync import SyncClient  # noqa: PLC0415 - lazy: pulls the web framework

    store = Store.open(args.out / "recall.sqlite")
    try:
        handed = run_jobs_once(store, SyncClient(args.url, token), data_root=args.out)
    finally:
        store.close()
    print(f"jobs: brought {handed} fleet job(s) home from {args.url}")
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


def _cmd_capture_mirror(args: argparse.Namespace) -> int:
    """Mirror the fleet's capture intent onto this Mac's mic (the Isis split). Polls the
    fleet every --interval seconds and applies pause/resume locally, reporting back what
    it applied. The token is RECALL_SYNC_TOKEN. Imports are lazy so `recall.cli` stays
    framework-free for the capture agents (recall.sync drags in the web framework)."""
    runlog.setup()  # timestamped intent-application logging to the agent's .err.log
    token = os.environ.get("RECALL_SYNC_TOKEN")
    if not token:
        print("capture-mirror needs RECALL_SYNC_TOKEN")
        return 1
    import time  # noqa: PLC0415

    from recall.capture_mirror import reconcile_once, run_loop  # noqa: PLC0415
    from recall.sync import SyncClient  # noqa: PLC0415 - lazy: pulls the web framework

    def note_applied(intent: str) -> None:
        # The durable "intent-seen" timestamp a resume timeline starts from
        # (recall capture-trace). Short-lived connection; a few rows a day.
        store = Store.open(_db_path(args.out))
        try:
            store.add_capture_event(
                capture_control.CaptureEventKind.MIRROR_APPLIED,
                utc=datetime.now(UTC),
                detail=intent or "running",
            )
        finally:
            store.close()

    client = SyncClient(args.url, token)
    if args.loop:
        run_loop(
            args.out,
            client,
            now=lambda: datetime.now(UTC),
            sleep=time.sleep,
            interval=args.interval,
            on_applied=note_applied,
        )
        return 0
    changed = reconcile_once(
        args.out, client, now=datetime.now(UTC), on_applied=note_applied
    )
    print(f"capture-mirror: {'applied fleet intent' if changed else 'no change'}")
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
    "sync": _cmd_sync,
    "jobs": _cmd_jobs,
    "pause": _cmd_pause,
    "resume": _cmd_resume,
    "capture-mirror": _cmd_capture_mirror,
    "capture-trace": _cmd_capture_trace,
    "repair-transcripts": _cmd_repair_transcripts,
    "verify": _cmd_verify,
    "index": _cmd_index,
    "transcribe": _cmd_transcribe,
    "reprocess": _cmd_reprocess,
    "worker": _cmd_worker,
    "beat-relay": _cmd_beat_relay,
    "live": _cmd_live,
    "score-asr": _cmd_score_asr,
    "reprobe": _cmd_reprobe,
    "coverage": _cmd_coverage,
    "search": _cmd_search,
    "show": _cmd_show,
    "transcript": _cmd_transcript,
    "correct": _cmd_correct,
    "redrive": _cmd_redrive,
    "refine": _cmd_refine,
    "scan-hallucinations": _cmd_scan_hallucinations,
    "scan-loops": _cmd_scan_loops,
    "scan-foreign-script": _cmd_scan_foreign_script,
    "scan-wordless": _cmd_scan_wordless,
    "llm-host": _cmd_llm_host,
    "score-attribution": _cmd_score_attribution,
    "enroll": _cmd_enroll,
    "identify": _cmd_identify,
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
