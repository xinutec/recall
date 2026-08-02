# Meeting recorder

A record-to-file mode inside the existing Android app, so a meeting or appointment is
captured by recall's own app and uploaded as a session — replacing the third-party mp3
recorder previously used for that job, whose ads are the reason to stop using it.

**Status: built** — `MeetingService`, `MeetingActivity`, `MeetingQueue`, `MeetingLibrary`,
`MeetingPlayer`, `MeetingUpload`.
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

## Nothing uploads until it has been listened to

A recording is evidence about an appointment, and whether it is worth keeping is a
judgement that can only be made after hearing it. So the recorder does not send anything
on its own: a finished recording appears in a list on the phone with **Play**, **Upload**
and **Delete**, and stays there until one of the last two is pressed.

Approval is a **move on disk**, not a flag — and so is every other state a recording can
be in, because each one is a decision or a verdict that has to survive a reboot, and a
rename is the only change that cannot half-happen:

| directory              | meaning                                            |
|------------------------|----------------------------------------------------|
| `meetings/`            | held: nothing sends it                             |
| `meetings/outbox/`     | approved — the only place the uploader looks       |
| `meetings/uploaded/`   | recall has it, and its length matches this copy    |
| `meetings/unverified/` | recall has it, but the lengths don't agree         |

The uploader looks only in the outbox, so it cannot send something that was never
approved, and a reboot between the decision and the upload cannot lose the decision.

Playback is deliberately small — one file, no queue, no service, released when the screen
closes. It is a check before uploading, not a media player, and it never touches the
device volume.

## A 2xx is not proof, so the lengths are compared

The server does check what arrives: `create_session` runs ffprobe on the uploaded file and,
if it can't read it, unlinks it and returns 400. Garbage cannot earn a success.

What that misses is a **post cut short mid-stream** — a dropped connection partway through
40 MB. ffprobe reports the duration of whatever showed up, so a truncated body still parses
and still returns 2xx, with seconds or minutes missing off the end. That is exactly the
case where the phone holds the only complete recording, and it was also the case where the
old behaviour deleted it.

So the response is read as a receipt. `create_session` returns `start` and `end`, so the
phone knows how long recall thinks the recording is, and compares it with the file it still
has. Shorter by more than 1.5 s — clear of two decoders rounding one file differently, well
under any real truncation — and it goes to `unverified/`, where the row says so next to the
Delete button. A length that can't be read on either side is *also* unverified: "couldn't
compare" must never render as "checked and fine".

## The upload queue

Meetings happen where the recall host is unreachable, and some guest networks block the
VPN outright, so there may be no route home from the building at all. So once a recording
*is* approved, delivery is offline-first: it waits in the outbox until the host answers.

- Write to `getExternalFilesDir(DIRECTORY_MUSIC)/meetings`, not `filesDir`. Both are
  app-private, but the former is visible over USB, so a recording whose upload never
  succeeds can still be retrieved by plugging the phone in.
- Upload via a `WorkManager` job with a network constraint and backoff. Each run drains
  the **whole outbox** rather than one named item, so a missed enqueue can't strand an
  approved recording: the files on disk are the state, not the job. A delivered recording
  leaves the outbox, so it is never sent twice and can't produce a duplicate session.
- The outbox is kicked on three "the host might be reachable now" events — a recording
  being approved, the screen opening, and the mic stream connecting (which proves we're
  home). All three use `REPLACE`, so a previous failure's backoff is abandoned rather
  than waited out.
- Each recording is a single file, `meeting-<local stamp>.ogg` — no title, no sidecar. The
  filename is the whole record, so a recording that ends in a crash still knows when it
  was made, and there is no second file to lose or keep in step.
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

The app knows the true start instant, so `start` is filled and the session lands at the
right point in the timeline rather than at the moment it happened to be uploaded.

## Scope

Phone: `MeetingService` mirroring `StreamService`'s lifecycle; `MeetingActivity` with
start/stop, elapsed time, the shared level meter and the list of what is still on the
phone; `MeetingQueue` (the files and the two directories), `MeetingLibrary`
(what the list shows, and the approve/delete actions), `MeetingPlayer` (listening back)
and `MeetingUpload` (the WorkManager job). The hosts moved to `SettingsActivity`, behind
the drawer, so the daily screen is status and Start/Stop rather than a form. Tests cover
the pure parts — file naming, start recovery, listing, approval, the row labels — as
`ShareUpload`'s time helpers were covered before.

Server: nothing.

## Decisions that were open, and how they went

- **Nothing is ever deleted automatically — not even after a successful upload.** This was
  the other way round at first, on the reasoning that the phone is the least reliable
  device and recall is what gets backed up (Isis's `recall-data-pvc` is rsynced nightly
  into odin's restic repo, with an off-site copy after). Both halves of that are true and
  it was still wrong, because of what a 2xx does *not* prove — see below. Deleting is now
  the one thing only a person does.
- **No pause/resume.** `MediaRecorder` supports it, but a paused recorder that is never
  resumed loses audio silently — the same failure the resume warning guards against for
  continuous capture. A break in a long appointment is Stop then Start, which costs a
  second session and, unlike silence, is visible.
- **The level meter reads `MediaRecorder.getMaxAmplitude()`**, polled every 100 ms, not
  `AudioRecord` frames — `MediaRecorder` owns the mic and never hands over the samples.
  Both modes scale it through the same `amplitudeLevel`, so the two meters read alike.

## No name, and the host it goes to

A recording has no title, on the phone or on the wire. The only thing worth knowing about
one before it is transcribed is when it happened, and the filename says that; the server
names the session `Meeting <date> <time>` from the `start` field. If it ever needs a name
it gets one in recall, where the transcript is to hand and the name can be chosen for what
the meeting turned out to be — rather than typed into a phone on the way into a waiting
room, which is the worst moment to be asked.

Uploads go to the **control host** (Isis), not the recorder host the PCM stream connects
to. `ShareActivity` used to post to the recorder host, which has served nothing on `:8000`
since the Mac's UI was retired in the Isis split — so sharing a recording in had been
silently failing. Both paths now use `Prefs.controlHost`.
