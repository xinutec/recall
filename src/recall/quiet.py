"""Find long total-quiet spans in the continuous capture — the mic heard nothing, for
this long — so they can be reviewed and deleted (most of the archive is pure waste).

**A minute's mean volume cannot tell speech from silence, and is not used to.** It was,
once, against a -60 dB line, and the archive disproves that line on every microphone:
the *quietest minute the speech detector actually heard speech in* averages -68.7 dB on
the USB mic, -83.2 on the pixel9, -85.1 on the pixel5 — all of them well under it. Of
course they do. A minute holding four seconds of far-field Dutch is fifty-six seconds of
silence, and the mean is the silence. A statistic that speech does not move is not a
speech detector, and no threshold on it can be made into one.

So the mean decides nothing here. It ranks and it draws; the vetoes are:

* **The speech detector is the authority.** A segment is sweepable only if the VAD
  listened to it and heard nothing (`speech_s == 0`). A segment nobody has listened to
  is *unknown*, not empty, and unknown is never swept.
* **A visible turn outranks everything.** A segment still bearing a current turn —
  machine or human — holds speech, whatever its volume says. A second line, and it must
  be: a reprocessing pass hides the turns it replaces, and a minute of real far-field
  Dutch was once found carrying no visible turn at all. The VAD sees the audio; this
  sees only the bookkeeping about it. Both must agree before anything is proposed.
* **Only what ASR has already examined.** An untranscribed segment is unknown too.
* **Per source.** Several mics record the same room at once (the USB mic and the
  phones), so segments interleave in time. A run is grouped *within* a source; mixing
  them would put one mic's files in another's span, and a span would then delete audio
  it never showed you.
* **Uploads are never swept, and a gap breaks a run.** An UPLOAD source is a purposely
  imported recording (a meeting), never idle room noise. And if capture stopped, the
  missing time is unknown, not quiet — without that rule a 4-minute hole could carry a
  run past the 5-minute bar on its own.

What survives is still only a *proposal*: the review UI draws the waveform (see
`recall.envelope`), because the VAD hears speech and not the bumps, coughs and shifting
about that a person may still want to keep. Those are shown, counted, and never hidden —
see `rank_spans` for what the list leads with. Deletion is always confirmed, and its
boundaries corrected, by a person; nothing here is automatic.
"""

from __future__ import annotations

from collections import defaultdict
from collections.abc import Callable, Sequence
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from recall.envelope import UNDECODABLE, SpanSound, encode_envelope, measure
from recall.ids import AudioSegmentId
from recall.sources import SourceKind
from recall.store import SegmentVolume, Store

# Only long runs are worth surfacing — a few seconds of quiet between utterances is
# normal speech rhythm, not waste. Default: 5 minutes.
MIN_QUIET_SPAN_S = 300.0
# Back-to-back capture segments abut within milliseconds; anything larger is a hole in
# the recording, which is unknown, not quiet, and so must break the run.
MAX_GAP_S = 2.0
# Continuous capture only. UPLOAD is an imported recording (a meeting) — the archive's
# most valuable audio, never a stream of idle noise, so never a delete candidate.
SWEEPABLE_KINDS = frozenset(SourceKind) - {SourceKind.UPLOAD}
# Concurrent decodes in a scan. The scan is ffmpeg-bound and ffmpeg is a subprocess, so
# these overlap: eight measured 2.6x the throughput of one. Kept modest so a scan cannot
# starve live capture and transcription, which share this machine.
SCAN_WORKERS = 8


@dataclass(frozen=True)
class QuietSpan:
    """A contiguous run of quiet segments from one source — a candidate for deletion."""

    source_id: str
    start: datetime
    end: datetime
    audio_ids: tuple[AudioSegmentId, ...]

    @property
    def duration_s(self) -> float:
        return (self.end - self.start).total_seconds()


def is_quiet(segment: SegmentVolume) -> bool:
    """Whether a segment holds no speech at all — the whole test, in one place.

    Every clause is a veto, and each has been seen to matter on the real archive:

    * **The speech detector heard nothing** (`speech_s == 0`). This is the one that
      counts. A segment nobody has listened to yet is *unknown*, not empty, and is
      never swept.
    * A segment that still bears a visible turn contains speech. Kept as a second line,
      but it cannot be the only one: a reprocessing pass hides the turns it replaces,
      and a minute of real far-field Dutch was found carrying no visible turn at all.
      The VAD sees the audio; this sees only the bookkeeping about it.
    * Unmeasured or untranscribed is unknown, and unknown is never deleted. (An
      unmeasured segment is also an unanalysed one, so the VAD clause would catch it
      anyway — but a file that will not decode must be vetoed by name, not by luck.)

    There is deliberately no volume clause. A threshold on a 60-second mean once lived
    here, and it vetoed nothing a real minute of speech would trip (see the module
    docstring) while shattering hour-long silences into unrankable shards wherever a
    door closed. Volume ranks these spans; it does not judge them.
    """
    return (
        segment.mean_db is not None
        and segment.transcribed
        and not segment.has_speech
        and segment.speech_s == 0.0
    )


def _source_spans(
    segments: Sequence[SegmentVolume],
    *,
    min_duration_s: float,
    max_gap_s: float,
) -> list[QuietSpan]:
    """Quiet runs within a single source's segments (time-ordered)."""
    spans: list[QuietSpan] = []
    run: list[SegmentVolume] = []

    def flush() -> None:
        if run and (run[-1].end - run[0].start).total_seconds() >= min_duration_s:
            spans.append(
                QuietSpan(
                    source_id=run[0].source_id,
                    start=run[0].start,
                    end=run[-1].end,
                    audio_ids=tuple(s.audio_id for s in run),
                )
            )

    for seg in segments:
        quiet = is_quiet(seg)
        contiguous = not run or (seg.start - run[-1].end).total_seconds() <= max_gap_s
        if quiet and contiguous:
            run.append(seg)
            continue
        flush()
        # A quiet segment after a gap doesn't end quiet — it starts the next run.
        run = [seg] if quiet else []
    flush()
    return spans


def find_quiet_spans(
    segments: Sequence[SegmentVolume],
    *,
    min_duration_s: float = MIN_QUIET_SPAN_S,
    max_gap_s: float = MAX_GAP_S,
) -> list[QuietSpan]:
    """Group each source's consecutive quiet segments (see `is_quiet`) into runs,
    keeping only spans at least `min_duration_s` long. Pure, so the grouping is
    unit-tested. Anything not provably empty — a segment the detector heard speech in,
    an unmeasured or untranscribed one, one bearing a turn, or a hole in the recording —
    breaks the run and stays out of it."""
    by_source: dict[str, list[SegmentVolume]] = defaultdict(list)
    for seg in segments:
        by_source[seg.source_id].append(seg)

    spans: list[QuietSpan] = []
    for source_segments in by_source.values():
        spans += _source_spans(
            sorted(source_segments, key=lambda s: s.start),
            min_duration_s=min_duration_s,
            max_gap_s=max_gap_s,
        )
    return sorted(spans, key=lambda s: (s.start, s.source_id))


def rank_spans(
    measured: Sequence[tuple[QuietSpan, SpanSound]],
) -> list[tuple[QuietSpan, SpanSound]]:
    """The review order: **the biggest first.** Pure, so the order is unit-tested.

    The list exists to reclaim disk, so it leads with the spans that reclaim the most.
    That sounds too obvious to state, and it is worth stating: this list was once ranked
    by *structure* — how featureless the audio is — which knows nothing of how long a
    span runs, so a spotless six-minute shard led a list containing a silent hour. It
    ranked the audio and forgot the point.

    Sound does not demote a span, it annotates one. A span's `sound_seconds` and its
    loudest moment are carried alongside for the UI to show: the detector heard no
    speech in any of this, but it does not hear a door closing or somebody turning over
    on the sofa, and only a person can say whether an hour of empty room with two thumps
    in it is worth keeping. Hiding it at the bottom of the list would not be caution, it
    would just be a slower way of not showing it. Structure breaks ties between equals.
    """
    return sorted(
        measured,
        key=lambda pair: (
            -pair[0].duration_s,
            pair[1].structure if pair[1].structure is not None else float("inf"),
        ),
    )


def scan_segments(
    store: Store,
    *,
    batch: int = 2000,
    workers: int = SCAN_WORKERS,
    should_stop: Callable[[], bool] | None = None,
) -> int:
    """Decode the sweepable segments not examined yet, caching each one's mean volume
    and envelope. Returns how many were examined this pass; call again while that's
    non-zero.

    One decode per file, once, ever: the archive is ~9k minute-long files, so nothing
    here may be recomputed on demand. A file that will not decode — truncated, corrupt,
    a stub left by a dying recorder — is recorded as UNDECODABLE rather than skipped:
    skipping it would leave it pending for ever, re-decoded by every scan, and the
    archive would never read as fully measured. Its volume stays NULL, so `is_quiet`
    vetoes it and it is never swept into a deletion; the review draws it as a gap.

    Decodes run several at a time. Measured: the work is ~99.5% ffmpeg (104 ms a file,
    against 0.5 ms of arithmetic), and ffmpeg is a subprocess, so the GIL is free while
    it runs — eight at once measures 2.6x the files a minute of one at a time. Writes
    stay on this thread, since the store is a single connection.

    `should_stop` is checked between chunks so a long scan can be cancelled promptly.
    """
    pending = store.audio_segments_unmeasured(limit=batch, kinds=SWEEPABLE_KINDS)
    examined = 0
    with ThreadPoolExecutor(max_workers=workers) as pool:
        for i in range(0, len(pending), workers):
            if should_stop is not None and should_stop():
                break
            chunk = pending[i : i + workers]
            results = pool.map(lambda item: measure(Path(item[1])), chunk)
            for (audio_id, _path), result in zip(chunk, results, strict=True):
                if result is None:
                    store.set_audio_measurement(audio_id, None, UNDECODABLE)
                else:
                    store.set_audio_measurement(
                        audio_id, result.mean_db, encode_envelope(result.buckets)
                    )
                examined += 1
    return examined


def quiet_spans(
    store: Store,
    *,
    min_duration_s: float = MIN_QUIET_SPAN_S,
) -> list[QuietSpan]:
    """The long total-quiet spans across the archive, from the cached volumes."""
    return find_quiet_spans(
        store.audio_segment_volumes(kinds=SWEEPABLE_KINDS),
        min_duration_s=min_duration_s,
    )
