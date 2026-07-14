"""The one-time migration of the system of record onto the fleet."""

from __future__ import annotations

import sqlite3
from datetime import UTC, datetime, timedelta
from pathlib import Path

from recall.seed import build_seed, fleet_path
from recall.sources import AudioSource, SourceKind
from recall.store import HUMAN_MODEL, Store
from recall.timeline import Segment

BASE = datetime(2026, 6, 13, 12, 0, 0, tzinfo=UTC)


def test_only_the_filename_survives_the_trip() -> None:
    # The fleet owns its layout — the same rule recall.sync applies to a pushed segment.
    assert (
        fleet_path("/Volumes/Backup/recall/usb/usb-20260613T170653.opus", "usb")
        == "/data/usb/usb-20260613T170653.opus"
    )


def _archive(root: Path, *, n: int = 3) -> Path:
    """A Mac-shaped archive: a DB whose paths are absolute, and the audio they name."""
    (root / "usb").mkdir(parents=True)
    db_path = root / "recall.sqlite"
    store = Store.open(db_path)
    store.add_source(
        AudioSource(id="usb", name="usb", kind=SourceKind.COREAUDIO, spec="")
    )
    for i in range(n):
        name = f"usb-2026061{i}T120000.opus"
        (root / "usb" / name).write_bytes(b"audio")
        audio_id = store.add_audio_segment(
            Segment(
                source_id="usb",
                sequence=i,
                start=BASE + timedelta(minutes=i),
                end=BASE + timedelta(minutes=i + 1),
                path=str(root / "usb" / name),  # absolute, on THIS machine
                sample_rate=48000,
                channels=1,
            )
        )
        # A segment with no machine turn at all — the delta protocol would never have
        # sent this one, and its audio would simply not exist on the fleet.
        if i == 1:
            store.add_transcript_segment(
                audio_segment_id=int(audio_id),
                start=BASE + timedelta(minutes=i),
                end=BASE + timedelta(minutes=i, seconds=5),
                text="a correction only a person could make",
                asr_model=HUMAN_MODEL,
            )
    store.close()
    return db_path


def test_the_seed_re_homes_every_audio_path(tmp_path: Path) -> None:
    root = tmp_path / "archive"
    db_path = _archive(root)
    seed_db = tmp_path / "seed" / "recall.sqlite"

    seed = build_seed(db_path, seed_db, archive_root=root)

    assert seed.segments == 3
    assert seed.rewritten == 3
    assert seed.missing_audio == ()

    db = sqlite3.connect(seed_db)
    paths = [r[0] for r in db.execute("SELECT path FROM audio_segments ORDER BY id")]
    db.close()
    assert paths == [
        "/data/usb/usb-20260610T120000.opus",
        "/data/usb/usb-20260611T120000.opus",
        "/data/usb/usb-20260612T120000.opus",
    ]


def test_the_seed_carries_what_the_delta_protocol_would_have_dropped(
    tmp_path: Path,
) -> None:
    """The reason this exists at all.

    `sync_push` sends only segments bearing a visible *machine* turn, and never sends a
    human turn. On the real archive that was 1,440 of 2,252 segments, no human turns at
    all (525 of them), and none of the corrections table. Audio can be re-transcribed; a
    person's correction cannot be re-made — and the whole point of moving the system of
    record off the Mac is that the Mac may die.
    """
    root = tmp_path / "archive"
    db_path = _archive(root)
    seed_db = tmp_path / "seed" / "recall.sqlite"

    build_seed(db_path, seed_db, archive_root=root)

    db = sqlite3.connect(seed_db)
    human = db.execute(
        "SELECT COUNT(*) FROM transcript_segments WHERE asr_model = ?", (HUMAN_MODEL,)
    ).fetchone()[0]
    segments = db.execute("SELECT COUNT(*) FROM audio_segments").fetchone()[0]
    db.close()

    assert human == 1  # the correction made the trip
    assert segments == 3  # including the two with no machine turn to carry them


def test_a_row_whose_audio_is_missing_is_reported_not_carried(tmp_path: Path) -> None:
    # On the fleet that row would be a recording that exists in the index and nowhere
    # else — a play button pointing at nothing. Say so; do not quietly rewrite it.
    root = tmp_path / "archive"
    db_path = _archive(root)
    (root / "usb" / "usb-20260611T120000.opus").unlink()
    seed_db = tmp_path / "seed" / "recall.sqlite"

    seed = build_seed(db_path, seed_db, archive_root=root)

    assert seed.rewritten == 2
    assert seed.missing_audio == (str(root / "usb" / "usb-20260611T120000.opus"),)


def test_a_model_name_that_looks_like_a_path_is_left_alone(tmp_path: Path) -> None:
    """`asr_model` holds strings like `/Volumes/Backup/recall/adapter-current` — the
    NAME of the model that produced a turn, not a file the fleet opens. 290 turns carry
    one. Rewriting them would forge the provenance of every one."""
    root = tmp_path / "archive"
    db_path = _archive(root)
    store = Store.open(db_path)
    audio_ids = store.audio_segment_paths()
    store.add_transcript_segment(
        audio_segment_id=int(audio_ids[0][0]),
        start=BASE,
        end=BASE + timedelta(seconds=2),
        text="transcribed by the fine-tuned adapter",
        asr_model=str(root / "adapter-current"),
    )
    store.close()
    seed_db = tmp_path / "seed" / "recall.sqlite"

    build_seed(db_path, seed_db, archive_root=root)

    db = sqlite3.connect(seed_db)
    models = [
        r[0]
        for r in db.execute(
            "SELECT asr_model FROM transcript_segments WHERE asr_model LIKE ?",
            (f"{root}%",),
        )
    ]
    db.close()
    assert models == [str(root / "adapter-current")]  # untouched
