# Meeting recorder (plan)

A record-to-file mode inside the existing Android app, so a meeting or appointment is
captured by recall's own app and uploaded as a session — replacing the third-party mp3
recorder currently used for that job, whose ads are the reason to stop using it.

**Status: planned, not built.** This file is the design decisions and why they were
made, so the plan survives being put down.

## Where it lives: `android/app`, as a second mode

Not a new app, and not the `android/web` WebView module.

- [`ShareUpload`](../android/app/src/main/kotlin/org/recall/mic/ShareUpload.kt) already
  posts to `/api/sessions` with a `start` instant, streamed. Reusing it means a recording
  made here arrives exactly like one shared from another app today.
- `Prefs` already holds the host, control host and device id. A second app would carry a
  second copy of that configuration, free to drift from the first.
- The hard parts exist: a microphone-type foreground service, the `UNPROCESSED` → `MIC`
  source fallback, the refused-foreground-start path, the notification, `LevelMeter`.
- Both modes want the one microphone. Inside a single process the rule can be enforced;
  across two apps the loser only learns about it as an `AudioRecord` init failure.
- A WebView can't hold a microphone-type foreground service, so `getUserMedia` there
  stops at screen-off.

## Format: Ogg/Opus — the container is the crash strategy

`MediaRecorder` with `OutputFormat.OGG` + `AudioEncoder.OPUS`, 48 kHz mono, audio source
`UNPROCESSED` falling back to `MIC` — the same preference `StreamService.openRecord`
makes, and for the same reason: automatic gain control and noise suppression damage the
speaker embeddings the diarizer depends on.

Ogg is chosen over m4a because **a truncated Ogg still decodes** to its last complete
page. A recording cut short by a flat battery or a crash costs its tail, not the whole
appointment. An m4a interrupted before `stop()` has no `moov` atom and is not
recoverable. That property is what makes one file per meeting acceptable, instead of
rolled parts plus a join.

Bitrate **48–64 kbps**, above the 32 kbps of continuous capture: a meeting is far-field
with several voices, and a one-off hour costs ~25 MB, so the storage argument behind
32 kbps does not apply here.

`.ogg` is already in `_UPLOAD_AUDIO_SUFFIXES` ([`src/recall/api.py`](../src/recall/api.py)),
so **no server change is needed** for any of this.

## The upload queue

Meetings happen where the recall host is unreachable, and some guest networks block the
VPN outright, so there may be no route home from the building at all. The recorder is
therefore offline-first: record to a file, queue it, upload when the host answers.

- Write to `getExternalFilesDir(DIRECTORY_MUSIC)`, not `filesDir`. Both are app-private,
  but the former is visible over USB, so a recording whose upload never succeeds can
  still be retrieved by plugging the phone in.
- Upload via a `WorkManager` job with a network constraint and backoff.
- The pending queue is **shown in the app** ("2 recordings waiting to upload"). A silent
  queue is how a lost recording goes unnoticed for weeks.

## Microphone contention

Starting a meeting recording stops `StreamService`; stopping it starts the stream again
if it was running. The meeting recorder wins because it is the deliberate act, and the
notification says why the stream stopped.

Household capture on the USB mic is a different device and is unaffected.

## What the session gets downstream

`create_session` registers an UPLOAD source holding **one segment for the whole file**,
so the recording is diarized as a single window — the regime that scores best on speaker
boundaries (see [pipeline.md](pipeline.md) §4). It appears in the web app immediately
with 0 turns while the worker transcribes and refine diarizes; rename, delete and
re-diarize already work on it.

Because the app knows the true start instant and can offer a title box, both the `start`
and `title` form fields can be filled — the share flow sets only `start`, leaving every
meeting named `Meeting <date> <time>`.

## Scope

Phone: a `MeetingService` mirroring `StreamService`'s lifecycle; a screen with
start/stop/pause, elapsed time, the existing level meter and a title field; the upload
queue. Tests cover the pure parts — file naming, queue transitions, start/title choice —
as `ShareUpload`'s time helpers are covered now.

Server: nothing.

## Open questions

- **Delete a recording from the phone once uploaded?** Recommended yes: the host archive
  is what gets backed up, and the queue only exists to reach it.
- **Should pause/resume be offered at all?** `MediaRecorder` supports it, and a break in
  a long appointment is real, but a paused recorder that is never resumed loses audio
  silently — the same failure the resume warning already guards against for capture.
