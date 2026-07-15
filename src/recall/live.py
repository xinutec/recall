"""Live, low-latency transcription via voice-activity detection.

Reads the mic (a second ffmpeg stream), detects utterances with Silero VAD, and
transcribes each one the moment the speaker pauses — latency ~2-3 s. These are
fast, *provisional* transcripts (asr_model = LIVE_MODEL, no archive file); the
worker's higher-quality archive pass supersedes them once it catches up
(recall.worker.reconcile_live).

The run loop is heavy/integration (lazy-imports torch/silero-vad/mlx and reads
the live device) so it is not exercised by tests; the WAV + teardown helpers are.
"""

from __future__ import annotations

import queue
import signal
import subprocess
import threading
import wave
from collections.abc import Callable
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import IO, Protocol

from recall.asr import DEFAULT_MODEL, mlx_transcribe, scratch_wav
from recall.loops import is_repetition_loop
from recall.sources import live_input_argv
from recall.store import LIVE_MODEL, Store
from recall.vocabulary import build_initial_prompt

_SAMPLE_RATE = 16000
_VAD_CHUNK = 512  # samples Silero expects per call at 16 kHz
_MIN_UTTERANCE_MS = 300
# Re-check the pause roughly once a second (in VAD chunks) so live stops promptly.
_STOP_CHECK_CHUNKS = _SAMPLE_RATE // _VAD_CHUNK


def mic_argv(device: str) -> list[str]:
    """The live input: ffmpeg reads the best-effort UDP tap that capture publishes
    (recall.sources.live_input_argv), NOT the mic. Only capture holds the CoreAudio
    device — two clients on one device starve each other — so live subscribes to the tap
    instead. `device` is unused now (capture owns device pinning); the parameter stays
    so the caller and the agent's `--device` arg are unchanged."""
    return live_input_argv()


class _Producer(Protocol):
    """The subprocess surface `_stop_producer` needs — `subprocess.Popen` satisfies it,
    and a fake satisfies it in tests."""

    def poll(self) -> int | None: ...
    def terminate(self) -> None: ...
    def wait(self, timeout: float | None = None) -> int: ...
    def kill(self) -> None: ...


def _stop_producer(producer: _Producer) -> None:
    """Tear the mic reader down for good: terminate, then hard-kill if it lingers.

    A live-agent stop MUST leave no reader behind. An orphaned mic process keeps the
    shared CoreAudio device open and wedges it to digital silence — which is what turned
    a live restart into a capture dead-window (the reader survived the agent's death and
    starved capture's own stream). terminate → wait → kill guarantees it's gone.
    """
    if producer.poll() is not None:
        return  # already exited
    producer.terminate()
    try:
        producer.wait(timeout=3)
    except subprocess.TimeoutExpired:
        producer.kill()
        producer.wait()


def write_wav(pcm: bytes, path: Path, *, sample_rate: int = _SAMPLE_RATE) -> None:
    """Write mono 16-bit PCM as a WAV file (for the transcriber)."""
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(sample_rate)
        wav.writeframes(pcm)


def drain_to_queue(
    stdout: IO[bytes], frames: queue.Queue[bytes | None], chunk_bytes: int
) -> None:
    """Read fixed-size PCM chunks off the mic pipe as fast as the OS delivers them,
    parking each on `frames`. This decouples *draining the producer* from VAD/whisper
    timing: if the consumer stalls (GC, a slow VAD call under load), this thread still
    empties the producer's stdout, so its CoreAudio input ring never overruns. That
    overrun (`coreaudio: unhandled buffer overrun. Data discarded`) is what silently
    dropped live speech under the old sox reader. A short read means the producer ended
    (EOF or a torn-down pipe): forward `None` as the end sentinel so the consumer stops.
    """
    try:
        while True:
            data = stdout.read(chunk_bytes)
            if len(data) < chunk_bytes:
                break
            frames.put(data)
    finally:
        frames.put(None)


def run_live(  # noqa: PLR0915, PLR0912 - cohesive streaming loop
    db_path: Path,
    *,
    work_dir: Path,
    model: str = DEFAULT_MODEL,
    device: str = "",
    should_stop: Callable[[], bool] | None = None,
) -> None:
    """Stream the mic, transcribe each utterance immediately, store it as live.

    Transcription runs in a background thread so the read loop never blocks —
    a blocked reader makes sox drop audio (buffer overrun). The thread owns its
    own store connection (sqlite connections are single-thread). When `should_stop`
    is supplied it's polled (~1 s) and the loop stops on a pause, releasing the mic.
    """
    import numpy as np  # noqa: PLC0415 - heavy, only for the live loop
    import torch  # noqa: PLC0415
    from silero_vad import VADIterator, load_silero_vad  # noqa: PLC0415

    work_dir.mkdir(parents=True, exist_ok=True)
    vad = VADIterator(load_silero_vad(), sampling_rate=_SAMPLE_RATE)

    utterances: queue.Queue[tuple[bytes, datetime] | None] = queue.Queue()

    def transcribe_loop() -> None:
        store = Store.open(db_path)
        try:
            while True:
                item = utterances.get()
                if item is None:
                    return
                pcm, start = item
                _emit(store, pcm, start, work_dir, model)
        finally:
            store.close()

    thread = threading.Thread(target=transcribe_loop, daemon=True)
    thread.start()

    producer = subprocess.Popen(mic_argv(device), stdout=subprocess.PIPE)
    if producer.stdout is None:  # pragma: no cover
        msg = "the mic producer produced no stdout"
        raise RuntimeError(msg)

    chunk_bytes = _VAD_CHUNK * 2
    min_bytes = _MIN_UTTERANCE_MS * _SAMPLE_RATE * 2 // 1000
    utterance = bytearray()
    in_speech = False
    samples_seen = 0
    utterance_start = 0
    anchor: datetime | None = None

    # A dedicated thread drains sox so a slow VAD/whisper step never backs up the mic
    # pipe and overruns CoreAudio (see drain_to_queue). The main loop consumes decoded
    # frames from here; `get(timeout=...)` keeps the pause check responsive when quiet.
    frames: queue.Queue[bytes | None] = queue.Queue()
    drain = threading.Thread(
        target=drain_to_queue, args=(producer.stdout, frames, chunk_bytes), daemon=True
    )
    drain.start()

    # launchd stops the agent with SIGTERM, whose default action exits Python WITHOUT
    # running the finally below — orphaning the mic reader to hold and wedge the shared
    # CoreAudio device. This handler tears the reader down first. Off the main thread
    # signal.signal raises ValueError off the main thread (never in the agent); the
    # finally below still runs regardless.
    def _handle_sigterm(*_: object) -> None:
        _stop_producer(producer)
        raise SystemExit(0)

    try:
        previous_sigterm = signal.signal(signal.SIGTERM, _handle_sigterm)
    except ValueError:
        previous_sigterm = None

    try:
        chunks = 0
        while True:
            chunks += 1
            if (
                should_stop is not None
                and chunks % _STOP_CHECK_CHUNKS == 0
                and should_stop()
            ):
                break  # a pause: stop reading and release the mic
            try:
                data = frames.get(timeout=1.0)
            except queue.Empty:
                continue  # no audio yet — loop back to re-check the pause
            if data is None:
                break  # producer ended (sox exited / pipe torn down)
            if anchor is None:
                anchor = datetime.now(UTC)
            frame = np.frombuffer(data, dtype=np.int16).astype(np.float32) / 32768.0
            event = vad(torch.from_numpy(frame))
            if in_speech:
                utterance += data
            if event and "start" in event:
                in_speech = True
                utterance = bytearray(data)
                utterance_start = samples_seen
            elif event and "end" in event:
                in_speech = False
                if len(utterance) >= min_bytes:
                    start = anchor + timedelta(seconds=utterance_start / _SAMPLE_RATE)
                    utterances.put((bytes(utterance), start))
                utterance = bytearray()
            samples_seen += _VAD_CHUNK
    finally:
        if previous_sigterm is not None:
            signal.signal(signal.SIGTERM, previous_sigterm)
        _stop_producer(producer)  # release the mic for good — never orphan it
        drain.join(timeout=5)  # producer gone → its stdout EOFs → the drain returns
        utterances.put(None)
        thread.join(timeout=30)


def _emit(
    store: Store, pcm: bytes, start: datetime, work_dir: Path, model: str
) -> None:
    with scratch_wav(work_dir / f"live-{start:%Y%m%dT%H%M%S%f}.wav") as clip:
        write_wav(pcm, clip)  # scratch — transcribed, then dropped on exit
        # Rebuilt per utterance: a vocabulary term added in the UI biases the very
        # next live transcription (short clips misspell names the most).
        result = mlx_transcribe(
            clip, model=model, initial_prompt=build_initial_prompt(store)
        )
    text = " ".join(s.text.strip() for s in result.segments if s.text.strip()).strip()
    if not text or is_repetition_loop(text):
        return  # empty or a degenerate loop (short live clips are loop-prone)
    duration = len(pcm) / 2 / _SAMPLE_RATE
    store.add_transcript_segment(
        audio_segment_id=None,
        start=start,
        end=start + timedelta(seconds=duration),
        text=text,
        asr_model=LIVE_MODEL,
        language=result.language,
    )
