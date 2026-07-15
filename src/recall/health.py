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

from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

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


def backup_check(age_hours: float | None, *, max_age_hours: float) -> Check:
    """The archive's only unrecoverable failure is losing the one local copy.

    Transcripts are derived views and can be rebuilt; the raw audio and the human
    corrections exist on one volume and nowhere else. A mirror that has silently stopped
    is therefore a `fail`, not a `warn` — it stopped once before, for nine days, and
    nothing said a word.
    """
    if age_hours is None:
        return Check(
            section="backup",
            label="off-machine mirror",
            verdict="fail",
            observed="never completed",
            expected=f"within {max_age_hours:.0f}h",
        )
    return Check(
        section="backup",
        label="off-machine mirror",
        verdict="pass" if age_hours <= max_age_hours else "fail",
        observed=f"last succeeded {age_hours:.1f}h ago",
        expected=f"within {max_age_hours:.0f}h",
        value=round(age_hours, 1),
        unit="h",
    )


def loss_check(losses: Sequence[Gap], dead_windows: int, *, window: timedelta) -> Check:
    """Did recorded speech go missing while capture was meant to be running?

    An unexplained gap (capture active, yet no audio landed) or a dead-window is lost,
    unrecoverable speech — the worst outcome the archive has — so it fails hard. Clean
    means every timeline gap in the window is accounted for by a deliberate pause. This
    is the reconciliation the archive lacked: a bare gap couldn't say whether audio was
    missing on purpose or because capture silently died (see recall.loss).
    """
    lost = sum((g.end - g.start for g in losses), timedelta())
    hours = round(window.total_seconds() / 3600)
    if losses or dead_windows:
        return Check(
            section="capture",
            label="speech-loss",
            verdict="fail",
            observed=(
                f"{len(losses)} unexplained gap(s) totalling {_minutes(lost)} min, "
                f"{dead_windows} dead-window(s) in {hours}h"
            ),
            expected="every gap explained by a deliberate pause",
            value=_minutes(lost),
            unit="min",
        )
    return Check(
        section="capture",
        label="speech-loss",
        verdict="pass",
        observed=f"no unexplained loss in {hours}h",
        expected="every gap explained by a deliberate pause",
        value=0.0,
        unit="min",
    )


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
            for path in directory.glob(f"{source_id}-*"):
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
