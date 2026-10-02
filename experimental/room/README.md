# room: the room stream, by hand

An experiment (#1388): for each minute, pick the microphone that heard it best
and transcribe that one minute once, instead of every microphone separately.
It is not part of production. recalld neither builds nor queues room minutes,
and no Mac agent runs this. What the fleet built before it left production
stays in `ingest.sqlite` as history.

Everything here runs on a local copy, so an experiment can never write to the
fleet.

## Run it

1. Copy the fleet's `ingest.sqlite` into a work directory with sqlite's
   `.backup`, read-only, never the live file. It holds the segment list, the
   levels the fleet measured, and the room history.
2. Fetch the window's clips. Reads `RECALL_SYNC_TOKEN`, as the runner does
   (`~/.config/recall/env`):

   ```sh
   room fetch --root DIR --from 2026-09-19T08:00:00Z --to 2026-09-19T12:00:00Z
   ```

3. Build the room minutes: measures levels for the window's clips, then picks a
   microphone per minute, writing `DIR/ingest/room/` and `room_blocks`.

   ```sh
   room build --root DIR --from ... --to ...
   ```

4. Transcribe, as whole minutes or cut at pauses (`--pieces`, see
   `src/pieces.rs`), with the ASR shim and the fleet's vocabulary:

   ```sh
   room transcribe --root DIR --from ... --to ... --out room.jsonl --pieces \
       --shim <ml-env python> -m recall.shim_asr
   ```

   Run from the repo root with `PYTHONPATH=src`, as the agents do. One JSON
   line per minute and arm; a rerun skips what `room.jsonl` already has.

5. Score against the Check page's corrections:

   ```sh
   python3 scripts/room_referee.py --db <recall.sqlite copy> \
       --room-results room.jsonl --arm pieces --out report.json
   ```

The work directory holds household audio and transcripts: keep it outside the
repo, and delete it when the experiment is done.

## What stays in production

The speech measurement (`segment_speech`) stays in recalld: liveness and the
queue's silence gate read it. The level scanner (`levels.rs`) and the
destroyed-audio detector (`processed.rs`) moved here, because the room builder
was their only reader.
