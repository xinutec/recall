"""The CLI is not a registrar: what kind does it claim for audio already on disk?

`recall index`, `recall transcribe` and `recall worker --id` are all handed a source id
for files someone put in the data root. None of them recorded that audio, so none of
them can know what produced it — and `Store.add_source` is INSERT OR IGNORE, so whatever
they claim is permanent until an authoritative registrar corrects it.

Claiming COREAUDIO there files a copied-in meeting as a microphone: it drops out of the
sessions list, is health-checked as a mic that has stopped, and — the one that costs
data — becomes SWEEPABLE, so the quiet review may delete it. `worker` with no `--id`
already answers DISCOVERED (worker.py, `process_all`); these tests hold the `--id`
branches and the two ingest commands to the same answer.

The assertions are on the *properties* that matter (not sweepable, not a device), not
just the enum name, because those are what a wrong kind actually costs.
"""

from __future__ import annotations

from pathlib import Path

from recall import cli
from recall.sources import (
    DEVICE_KINDS,
    SWEEPABLE_KINDS,
    AudioSource,
    SourceKind,
)
from recall.store import Store

MEETING = "meeting-20260803-1034"


def _kind(root: Path, source_id: str) -> SourceKind | None:
    store = Store.open(root / "recall.sqlite")
    try:
        return store.source_kind(source_id)
    finally:
        store.close()


def _audio_dropped_in(root: Path, source_id: str) -> None:
    """Someone copied a recording into the data root. No agent, no handshake."""
    (root / source_id).mkdir(parents=True)


def test_index_claims_discovered_for_audio_it_did_not_record(tmp_path: Path) -> None:
    _audio_dropped_in(tmp_path, MEETING)

    assert cli.main(["index", "--out", str(tmp_path), "--id", MEETING]) == 0

    assert _kind(tmp_path, MEETING) is SourceKind.DISCOVERED


def test_transcribe_claims_discovered_for_audio_it_did_not_record(
    tmp_path: Path,
) -> None:
    _audio_dropped_in(tmp_path, MEETING)

    assert cli.main(["transcribe", "--out", str(tmp_path), "--id", MEETING]) == 0

    assert _kind(tmp_path, MEETING) is SourceKind.DISCOVERED


def test_worker_id_branch_agrees_with_the_all_sources_branch(tmp_path: Path) -> None:
    # `worker` with no --id registers DISCOVERED via process_all; --id must not
    # disagree with it about the same directory of audio.
    _audio_dropped_in(tmp_path, MEETING)

    assert cli.main(["worker", "--out", str(tmp_path), "--id", MEETING, "--basic"]) == 0

    assert _kind(tmp_path, MEETING) is SourceKind.DISCOVERED


def test_an_indexed_meeting_is_not_deletable_and_is_not_a_microphone(
    tmp_path: Path,
) -> None:
    # The two costs of the wrong kind, asserted directly.
    _audio_dropped_in(tmp_path, MEETING)
    cli.main(["index", "--out", str(tmp_path), "--id", MEETING])

    kind = _kind(tmp_path, MEETING)
    assert kind not in SWEEPABLE_KINDS, "a copied-in meeting must not be sweepable"
    assert kind not in DEVICE_KINDS, "a copied-in meeting is not a recording device"


def test_the_real_registrar_still_corrects_the_cli_guess(tmp_path: Path) -> None:
    # DISCOVERED is an admission, not a verdict: whoever knows must still win.
    _audio_dropped_in(tmp_path, MEETING)
    cli.main(["index", "--out", str(tmp_path), "--id", MEETING])

    store = Store.open(tmp_path / "recall.sqlite")
    try:
        store.register_source(
            AudioSource(id=MEETING, name=MEETING, kind=SourceKind.UPLOAD, spec="")
        )
    finally:
        store.close()

    assert _kind(tmp_path, MEETING) is SourceKind.UPLOAD


def test_indexing_does_not_downgrade_a_registered_microphone(tmp_path: Path) -> None:
    # add_source is INSERT OR IGNORE, so indexing the usb directory must never
    # overwrite the capture agent's authoritative COREAUDIO with an admission.
    _audio_dropped_in(tmp_path, "usb")
    store = Store.open(tmp_path / "recall.sqlite")
    try:
        store.register_source(
            AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
        )
    finally:
        store.close()

    cli.main(["index", "--out", str(tmp_path), "--id", "usb"])

    assert _kind(tmp_path, "usb") is SourceKind.COREAUDIO
