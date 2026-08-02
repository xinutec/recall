# Meeting recorder

A record-to-file mode inside the existing Android app, so a meeting or appointment is
captured by recall's own app and uploaded as a session — replacing the third-party mp3
recorder previously used for that job, whose ads are the reason to stop using it.

**Status: built** (`MeetingService`, `MeetingActivity`, `MeetingQueue`, `MeetingUpload`).
This file is the design decisions and why they were made.

**It does not record mp3, and cannot:** Android has no MP3 encoder — `MediaRecorder` and
`MediaCodec` decode the format but have never encoded it. Emitting `.mp3` would mean
bundling LAME through the NDK. Opus is what the platform encodes well, and the server
accepts it, so the choice below costs nothing.

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

Opus encoding arrived in Android 10, so meeting recording requires API 29 and says so on
anything older. `minSdk` stays 26: an older phone can still do the streaming job, and
quietly writing an m4a instead would put an unrecoverable container exactly where the
crash strategy expects a recoverable one.

## The upload queue

Meetings happen where the recall host is unreachable, and some guest networks block the
VPN outright, so there may be no route home from the building at all. The recorder is
therefore offline-first: record to a file, queue it, upload when the host answers.

- Write to `getExternalFilesDir(DIRECTORY_MUSIC)/meetings`, not `filesDir`. Both are
  app-private, but the former is visible over USB, so a recording whose upload never
  succeeds can still be retrieved by plugging the phone in.
- Upload via a `WorkManager` job with a network constraint and backoff. Each run drains
  the **whole** queue rather than one item, so a missed enqueue can't strand a recording:
  the files on disk are the state, not the job.
- The queue is kicked on three "the host might be reachable now" events — the screen
  opening, a recording finishing, and the mic stream connecting (which proves we're home).
  All three use `REPLACE`, so a previous failure's backoff is abandoned rather than
  waited out.
- Each recording is `meeting-<local stamp>.ogg` plus a `.ogg.json` sidecar holding the
  title and true start. The sidecar is written **before the first audio frame**, so a
  recording that ends in a crash still knows what it is; if it's lost anyway, the start is
  recovered from the filename and the title falls back to the server's default.
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

Phone: `MeetingService` mirroring `StreamService`'s lifecycle; `MeetingActivity` with
start/stop, elapsed time, the shared level meter and a title field; `MeetingQueue` (the
files) and `MeetingUpload` (the WorkManager job). Tests cover the pure parts — file
naming, start recovery, queue listing, the title's wire format — as `ShareUpload`'s time
helpers were covered before.

Server: nothing.

## Decisions that were open, and how they went

- **The recording is deleted from the phone once the host has it.** The host archive is
  what gets backed up, and the queue only exists to reach it. Nothing is deleted before a
  2xx.
- **No pause/resume.** `MediaRecorder` supports it, but a paused recorder that is never
  resumed loses audio silently — the same failure the resume warning guards against for
  continuous capture. A break in a long appointment is Stop then Start, which costs a
  second session and, unlike silence, is visible.
- **The level meter reads `MediaRecorder.getMaxAmplitude()`**, polled every 100 ms, not
  `AudioRecord` frames — `MediaRecorder` owns the mic and never hands over the samples.
  Both modes scale it through the same `amplitudeLevel`, so the two meters read alike.

## The `title` field, and the host it goes to

The app knows the true start instant and can offer a title box, so both the `start` and
`title` form fields are filled — the share flow sets only `start`, leaving every meeting
named `Meeting <date> <time>`.

Uploads go to the **control host** (Isis), not the recorder host the PCM stream connects
to. `ShareActivity` used to post to the recorder host, which has served nothing on `:8000`
since the Mac's UI was retired in the Isis split — so sharing a recording in had been
silently failing. Both paths now use `Prefs.controlHost`.
