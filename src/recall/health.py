"""Is the recording actually recording? — the check recall never had.

Capture can die. On 22 June it crash-looped: fourteen start attempts between 01:05 and
03:10, an hour and a half of nothing recorded, and *nobody knew*. It was found three
weeks later by diffing the filesystem against the database by hand. launchd restarts
capture when it dies (KeepAlive), which is why a persistent fault becomes a loop rather
than a stop — and a loop looks, from the outside, exactly like a quiet house.

So this asks the only question that matters: **is audio still landing on disk?**

It reads the *filesystem*, not the database, deliberately. A segment file appears
every 60 seconds while capture lives; the transcription pipeline behind it can be
hours behind without any of that meaning the microphone stopped. Asking the pipeline
whether the recorder is alive conflates two failures that need different answers.

The verdicts are not uniform, because the recorders are not:

* the **always-on mic** (the USB condenser, wired to this machine) has no excuse for
  silence. If it stops, recording has stopped: `fail`.
* a **phone** is carried out of the house, runs out of battery, has its app closed. Its
  silence is normal life, so it `warn`s — loud enough to see, too quiet to cry wolf.
* **every source silent at once** is not three coincidences. It is the capture process,
  or the machine, and it is the loudest thing this module can say.

A *paused* recording is not a broken one: everything reports `skip`, with the resume
time shown, so a deliberate pause reads as deliberate.
"""

from __future__ import annotations

from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

from recall.capture import segment_glob
from recall.sources import SourceKind
from recall.timeline import Gap

# A live capture writes a segment file every 60 seconds. Two missed rotations is noise
# (a slow disk, a rotation straddling the check); five is not.
SILENT_AFTER = timedelta(minutes=5)
# The always-on mic is the one that must never be silent — it is wired to the machine
# doing the recording. A phone leaves the house; this does not.
ALWAYS_ON = SourceKind.COREAUDIO


@dataclass(frozen=True)
class Recorder:
    """One microphone, and when audio last landed on disk from it."""

    source_id: str
    kind: SourceKind
    last_audio: datetime | None


@dataclass(frozen=True)
class Check:
    """One fleetwatch check — see its report contract (README, "The report contract").

    `label` is the trend identity and must stay stable across runs; anything that varies
    per run belongs in `observed` or `value`.
    """

    section: str
    label: str
    verdict: str  # pass | warn | fail | skip
    observed: str
    expected: str
    value: float | None = None
    unit: str | None = None


def _minutes(since: timedelta) -> float:
    return round(since.total_seconds() / 60.0, 1)


def capture_checks(
    recorders: Sequence[Recorder],
    *,
    now: datetime,
    paused_until: datetime | None = None,
    silent_after: timedelta = SILENT_AFTER,
) -> list[Check]:
    """What fleetwatch should be told about the recording, right now.

    Pure — the filesystem read happens in `recorders_on_disk`, so the rules that decide
    whether a household has stopped being recorded are testable without a microphone.
    """
    expected = f"audio within {_minutes(silent_after):.0f} min"

    if paused_until is not None and paused_until > now:
        # Deliberate. Not a fault, and it must never page anyone — but it is shown,
        # because a pause nobody remembers is how a week of memory goes missing.
        resume = paused_until.isoformat(timespec="minutes")
        return [
            Check(
                section="capture",
                label="recording",
                verdict="skip",
                observed=f"paused until {resume}",
                expected=expected,
            )
        ]

    checks: list[Check] = []
    silent: list[str] = []
    for recorder in sorted(recorders, key=lambda r: r.source_id):
        if recorder.last_audio is None:
            since = None
            quiet = True
        else:
            since = now - recorder.last_audio
            quiet = since >= silent_after
        if quiet:
            silent.append(recorder.source_id)

        if not quiet:
            verdict = "pass"
        elif recorder.kind == ALWAYS_ON:
            verdict = "fail"  # wired to this machine: silence means it stopped
        else:
            verdict = "warn"  # a phone: out of the house, flat battery, app closed

        checks.append(
            Check(
                section="capture",
                label=recorder.source_id,
                verdict=verdict,
                observed=(
                    "no audio ever recorded"
                    if since is None
                    else f"last audio {_minutes(since):.1f} min ago"
                ),
                expected=expected,
                value=None if since is None else _minutes(since),
                unit="min",
            )
        )

    # The summary check, and the one that catches what actually happened in June: three
    # microphones do not fall silent together by coincidence. That is the capture
    # process, or the machine it runs on.
    everything = bool(recorders) and len(silent) == len(recorders)
    checks.append(
        Check(
            section="capture",
            label="recording",
            verdict="fail" if everything or not recorders else "pass",
            observed=(
                "no recorders found"
                if not recorders
                else "every microphone is silent — capture is not running"
                if everything
                else f"{len(recorders) - len(silent)}/{len(recorders)} microphones live"
            ),
            expected=expected,
        )
    )
    return checks


# How long the archive-reading half of the doctor may take before it is treated as
# not having answered. A healthy run is ~1.5s end to end, so this is forty times the
# work — and it has to stay far under the agent's 300s StartInterval, because
# `KeepAlive = false` means launchd will not start the next doctor while this one is
# still going: one wedged run silences every run after it.
ARCHIVE_BOUND = timedelta(seconds=60)
# And the reading that predicts the bound being hit. During the 2026-08-10 starvation
# a two-table COUNT(*) alone took 4m19s, so the interesting range is not near 1.5s;
# anything past a few seconds means the volume is already contended.
ARCHIVE_SLOW = timedelta(seconds=10)


def archive_check(
    seconds: float | None,
    *,
    detail: str = "",
    bound: timedelta = ARCHIVE_BOUND,
    slow: timedelta = ARCHIVE_SLOW,
) -> Check:
    """Could this machine read its own archive at all — and how long did it take?

    Every other check in this module presumes the archive answered. On 2026-08-10
    it did not, for over an hour, and the doctor did not report that: it lives on
    the volume it checks, so it went into uninterruptible disk wait alongside the
    worker and the sync (#709). This is the check that survives that, because the
    archive is read in a child process the parent abandons (`recall.bounded`) and
    the timeout is reported from off the disk.

    ⚠ **Unanswered is `fail`, never `skip`.** A skip reads as "not applicable",
    and nothing is more applicable than the archive being unreachable — the June
    lesson was that a silence which looks deliberate is how a fault survives for
    weeks. The latency is carried as `value` so the trend is visible while it is
    still only slow, which is the only warning anyone gets before it wedges.
    """
    expected = f"the archive read in under {slow.total_seconds():.0f}s"
    if seconds is None:
        return Check(
            section="archive",
            label="archive answers",
            verdict="fail",
            observed=detail or f"no answer in {bound.total_seconds():.0f}s",
            expected=expected,
        )
    observed = f"read in {seconds:.1f}s"
    if detail:
        return Check(
            section="archive",
            label="archive answers",
            verdict="fail",
            observed=f"{detail} (after {seconds:.1f}s)",
            expected=expected,
        )
    return Check(
        section="archive",
        label="archive answers",
        verdict="warn" if seconds >= slow.total_seconds() else "pass",
        observed=(
            observed
            if seconds < slow.total_seconds()
            else f"{observed} — something else owns the disk"
        ),
        expected=expected,
        value=round(seconds, 2),
        unit="s",
    )


def agent_checks(agents: Sequence[tuple[str, bool]]) -> list[Check]:
    """One check per launchd agent. An installed-but-unloaded agent is always a fault:
    the agents self-gate (they park while capture is paused, they do not unload), so
    "not loaded" never means "deliberately off"."""
    if not agents:
        return [
            Check(
                section="agents",
                label="installed",
                verdict="fail",
                observed="no recall agents installed",
                expected="every agent loaded",
            )
        ]
    return [
        Check(
            section="agents",
            label=label,
            verdict="pass" if loaded else "fail",
            observed="loaded" if loaded else "NOT LOADED",
            expected="loaded",
        )
        for label, loaded in sorted(agents)
    ]


def mirror_check(unmirrored: int, *, slack: timedelta) -> Check:
    """Is the fleet's copy of the archive complete?

    The invariant since the mirror-completion push: every processed segment reaches
    the fleet within a couple of sync passes. A count stuck above zero (beyond the
    in-flight slack) means the mirror has silently stopped — the same class of
    failure as a stalled backup, and it gets the same verdict: `fail`, because "if
    the Mac dies the archive lives on Isis" is only true while this is zero.
    """
    slack_min = slack.total_seconds() / 60.0
    return Check(
        section="sync",
        label="fleet mirror complete",
        verdict="pass" if unmirrored == 0 else "fail",
        observed=(
            "every processed segment mirrored"
            if unmirrored == 0
            else f"{unmirrored} processed segment(s) not on the fleet"
        ),
        expected=f"0 unmirrored older than {slack_min:.0f}m",
        value=float(unmirrored),
        unit="segments",
    )


def sweep_refusal_check(refused: int) -> Check:
    """Has the Mac declined any fleet sweep?

    A sweep is a deletion the system of record asks the Mac to apply to its master
    archive. The Mac honours one only when its own VAD already scored the segment
    speechless — so a refusal means Isis asked to delete audio the Mac measured as
    real speech. In normal operation that never happens: every legitimate quiet-review
    deletion is of audio both machines saw as idle. A non-zero count therefore reads
    as a compromised or misbehaving fleet trying to reach protected audio — `warn`,
    not `fail`, because the audio was *kept*: the invariant held, this is the alarm
    that it was tested.
    """
    return Check(
        section="sync",
        label="fleet sweeps honoured",
        verdict="pass" if refused == 0 else "warn",
        observed=(
            "no sweep refused"
            if refused == 0
            else f"{refused} fleet sweep(s) refused — kept audio the Mac scored speech"
        ),
        expected="0 refusals",
        value=float(refused),
        unit="refusals",
    )


# NO backup_check here — the Mac does not perform the off-machine backup and so must
# not claim to observe one. odin's nightly restic pulls recall from Isis (an
# integrity-checked SQLite snapshot taken inside the pod, plus the audio PVC) and
# reports its own success; asserting it here would check someone else's machine,
# and the Mac's own copy is the master the backup is taken *of*.


_LOSS_EXPECTED = "every gap explained by a deliberate pause"
# Worst-wins, for the roll-up. `skip` ranks with `pass`: a deliberate pause is not a
# fault, and must never drag the summary upward.
_SEVERITY = {"pass": 0, "skip": 0, "warn": 1, "fail": 2}


def _loss_summary(gaps: int, lost: timedelta, dead: int) -> str:
    parts = []
    if gaps:
        parts.append(f"{gaps} unexplained gap(s) totalling {_minutes(lost)} min")
    if dead:
        parts.append(f"{dead} dead-window(s)")
    return ", ".join(parts)


def _loss_verdict(kind: SourceKind | None) -> str:
    """How loudly to say that this microphone lost speech.

    The same rule `capture_checks` applies to silence: the always-on mic is wired to
    this machine and has no excuse, a phone is carried out of the house and has several.
    An unknown device gets the strict verdict — no excuse has been established for it.
    """
    return "warn" if kind is not None and kind is not ALWAYS_ON else "fail"


def loss_checks(
    losses: Sequence[Gap],
    dead_windows: Mapping[str, int],
    sources: Sequence[tuple[str, SourceKind]],
    *,
    window: timedelta,
) -> list[Check]:
    """Did recorded speech go missing while capture was meant to be running — and on
    which microphone?

    An unexplained gap (capture active, yet no audio landed) or a dead-window is lost,
    unrecoverable speech — the worst outcome the archive has. Clean means every timeline
    gap in the window is accounted for by a deliberate pause. This is the reconciliation
    the archive lacked: a bare gap couldn't say whether audio was missing on purpose or
    because capture silently died (see recall.loss).

    Reported **per device**, like `capture_checks` and `agent_checks` before it, because
    a single collapsed count answers the wrong question. It says the house lost speech;
    it cannot say which microphone to go and fix, and it cannot tell a phone that was
    carried away from the wired mic that has no excuse. A dead phone then reads as
    loudly as a dead archive, which is how four dead windows on one pixel9 held the
    whole check red for three days while every other mic recorded perfectly.

    The roll-up keeps the bare `speech-loss` label so its trend survives the split, and
    takes the worst verdict across devices. Per-device labels are qualified
    (`speech-loss:usb`) because fleetwatch's mute key is `(source, collector, label)` —
    label is unique within a collector, and the bare source_id is already taken by the
    per-mic recording checks (see migrations/0003_mutes.sql in fleetwatch).
    """
    hours = round(window.total_seconds() / 3600)
    kinds = dict(sources)
    gaps_by_source: dict[str, list[Gap]] = {}
    for gap in losses:
        gaps_by_source.setdefault(gap.source_id, []).append(gap)

    # Every registered device, plus any that lost speech without being one: loss on an
    # unknown source must get its own line rather than vanish into the roll-up.
    source_ids = sorted(set(kinds) | set(dead_windows) | set(gaps_by_source))

    checks: list[Check] = []
    hurt: list[str] = []
    total_lost = timedelta()
    for source_id in source_ids:
        gaps = gaps_by_source.get(source_id, [])
        dead = dead_windows.get(source_id, 0)
        lost = sum((g.end - g.start for g in gaps), timedelta())
        total_lost += lost
        if gaps or dead:
            summary = _loss_summary(len(gaps), lost, dead)
            hurt.append(f"{source_id}: {summary}")
            verdict = _loss_verdict(kinds.get(source_id))
            observed = f"{summary} in {hours}h"
        else:
            verdict = "pass"
            observed = f"no unexplained loss in {hours}h"
        checks.append(
            Check(
                section="capture",
                label=f"speech-loss:{source_id}",
                verdict=verdict,
                observed=observed,
                expected=_LOSS_EXPECTED,
                value=_minutes(lost),
                unit="min",
            )
        )

    checks.append(
        Check(
            section="capture",
            label="speech-loss",
            verdict=max(
                (c.verdict for c in checks), key=lambda v: _SEVERITY[v], default="pass"
            ),
            observed="; ".join(hurt) if hurt else f"no unexplained loss in {hours}h",
            expected=_LOSS_EXPECTED,
            value=_minutes(total_lost),
            unit="min",
        )
    )
    return checks


def recorders_on_disk(
    root: Path, sources: Sequence[tuple[str, SourceKind]], *, now: datetime
) -> list[Recorder]:
    """When each microphone last wrote audio, read from the archive directory itself.

    The newest *non-empty* file's mtime, not the database: capture writing to disk is
    the fact under test, and the pipeline that indexes those files can be far behind
    without the microphone having missed a second. Zero-byte segments are skipped —
    capture can run and roll a fresh file every segment while the device delivers only
    digital silence (a coreaudio startup dead-window), and those empty stubs must not
    read as "recording": counting them is how a 13-minute dead window looked healthy.
    """
    recorders = []
    for source_id, kind in sources:
        directory = root / source_id
        newest: float | None = None
        if directory.is_dir():
            for path in segment_glob(directory, source_id):
                try:
                    stat = path.stat()
                except OSError:
                    continue  # vanished mid-scan; the next pass will see it
                if stat.st_size == 0:
                    continue  # dead stub — capture rolled a file but caught no audio
                if newest is None or stat.st_mtime > newest:
                    newest = stat.st_mtime
        recorders.append(
            Recorder(
                source_id=source_id,
                kind=kind,
                last_audio=(
                    None
                    if newest is None
                    else datetime.fromtimestamp(newest, tz=now.tzinfo)
                ),
            )
        )
    return recorders
