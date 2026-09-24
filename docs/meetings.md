# Meeting recordings

A meeting or appointment is one recording, not the continuous room. It becomes
a *session*: its own source, transcribed and diarized by the same runners every
microphone clip goes through, read back as a clean attributed transcript.

## Getting one in

- **Record it on the phone** with the app's meeting recorder
  ([meeting-recorder.md](meeting-recorder.md)); it uploads itself as a session.
- **Or upload any file** on the app's Sessions page (`POST /api/sessions`:
  mp3, m4a, wav, flac, ogg, opus, webm). The session appears at once with zero
  turns; the runners fill it in.

A session's id is its local start, `meeting-YYYYMMDD-HHMM` (Europe/London), and
its audio lands in the ingest plane like any delivered blob. Uploading the same
recording twice is a no-op on the id.

## Speakers

Two columns both get called "the speaker":

- `speaker_cluster` is diarization's answer (`SPEAKER_00`, `SPEAKER_01`),
  written by the machine on every turn.
- `speaker_label` is the name a person gave: to a whole voice in the session
  screen's "Who's speaking" strip, which enrols it, or to one line.

A null `speaker_label` means nobody has named the voices yet, not that
diarization failed. **Re-diarize** on a session re-queues its clips for the
voices runner and the pass decides afresh against the turns standing now.

## Reading it

The session screen in the app, or from a terminal:

```sh
recall-cli sessions
recall-cli transcript meeting-20260209-1033
```

Diarized output on a long recording is a rough dump: turns can be
mis-assigned, and the second half can collapse into one run-on block. A
publishable transcript is corrected in the app (speakers from the content,
names, drug names, numbers) and then exported; the export is deterministic, so a
re-run with no new corrections produces no diff. Meeting quality has not been
measured (#1470); the audio remains the source of truth.
