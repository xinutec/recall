# Meeting recorder

A record-to-file mode in the Android app (`MeetingService`, `MeetingActivity`,
`MeetingQueue`, `MeetingLibrary`, `MeetingPlayer`, `MeetingUpload`); a recording
is uploaded as a session. This file holds the decisions and their reasons.

## In `android/app`, not a new app

It shares `Prefs`, the mic-type foreground service, the `UNPROCESSED` → `MIC`
fallback and `ShareUpload` (`POST /api/sessions` with a `start` instant). Both
modes want the one microphone, and only one process can enforce that: starting a
meeting stops `StreamService`, stopping it restarts the stream if it was on. A
WebView cannot hold a mic foreground service.

## Ogg/Opus, 56 kbps, 48 kHz mono

A truncated Ogg decodes to its last complete page, so a crash or flat battery
costs the tail, not the meeting; an m4a cut before `stop()` has no `moov` atom.
That is why one file per meeting is acceptable. Android has no MP3 encoder.
Opus needs Android 10 (API 29); older phones only stream (`minSdk` 26), rather
than fall back to m4a. The server already accepts `.ogg`
(`recalld/src/upload.rs`, `AUDIO_SUFFIXES`).

## Nothing uploads unheard, nothing is deleted automatically

A recording waits on the phone with Play, Upload and Delete. Every state is a
directory, because a rename cannot half-happen and survives a reboot:

| directory              | meaning                                  |
|------------------------|------------------------------------------|
| `meetings/`            | held                                     |
| `meetings/outbox/`     | approved: the only place the uploader looks |
| `meetings/uploaded/`   | recall has it, same length               |
| `meetings/unverified/` | recall has it, lengths disagree or unreadable |

Delete is the only removal, even after a verified upload. Files live in
`getExternalFilesDir(DIRECTORY_MUSIC)/meetings`, reachable over USB.

## A 2xx is not proof

recalld ffprobes an upload and rejects what it cannot read, but a post cut short
mid-stream still parses and returns 2xx. So the phone compares the `start`/`end`
the response reports with its file: more than 1.5 s shorter
(`MeetingQueue.LENGTH_TOLERANCE_MS`), or unreadable on either side, is
unverified.

## The upload queue

Meetings happen where recall is unreachable, so delivery waits in the outbox. A
WorkManager job (network constraint, backoff) drains the whole outbox, so a
missed enqueue strands nothing and a delivered file is never sent twice. It is
kicked, with `REPLACE` to drop a previous backoff, when a recording is approved,
the screen opens, or the mic stream connects. The app shows how many wait.

One file per recording, `meeting-<local stamp>.ogg`, no title or sidecar; the
server names the session from `start`, and a name, if wanted, is given in recall.

## Credentials

`POST /api/sessions` sits behind the web sign-in, which a phone cannot do. The
phone sends `RECALL_DEVICE_TOKEN` as a bearer (Settings, "upload token"; blank
sends none), accepted on `POST /api/sessions` only: not the sync token, which
opens all of `/sync/*`, and not the login-free list, which suits a pause button
but not a 40 MB upload. `POST /api/devices/outbox` is login-free, since it
reports failed uploads, a bad token included.

Uploads go to the control host (`Prefs.controlHost`), not the recorder host.

## Downstream

The session is one `upload` source holding one segment for the whole file, so
it is diarized as a single window ([architecture.md](architecture.md)). It
appears at once with no turns and lands at its true start on the timeline.

## Not done, on purpose

- No pause/resume: a paused recorder never resumed loses audio silently; Stop
  then Start is visible.
- The level meter polls `MediaRecorder.getMaxAmplitude()`, as `MediaRecorder`
  owns the mic, scaled by the shared `amplitudeLevel`.
