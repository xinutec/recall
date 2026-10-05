# Meeting recordings

A meeting is one recording, not the continuous room. It becomes a *session*: its
own source, transcribed and diarized by the same runners as every microphone
clip.

## Getting one in

- The phone's meeting recorder ([meeting-recorder.md](meeting-recorder.md)).
- Any file on the Sessions page (`POST /api/sessions`: mp3, m4a, wav, flac,
  ogg, opus, webm). The session appears at once, empty, and the runners fill it.

The id is the local start in Europe/London, `meeting-YYYYMMDD-HHMM`
(`upload.rs`); uploading the same recording twice is a no-op.

**Language.** Unpinned, Whisper guesses per recording and can come back
translated (Dutch as English or Italian). Pin Dutch or English at upload or from
the session's menu (`sessions::LANGUAGES`); pinning later transcribes again and
sets the old lines aside. No automatic guess measured better (#1470).

## Speakers

- `speaker_cluster`: diarization's answer (`SPEAKER_00`, ...), on every turn.
- `speaker_label`: a name a person gave, to a voice in the "Who's speaking"
  strip (which enrols it) or to one line. Null means nobody named it yet.

**Re-diarize** re-queues the session's clips for the voices runner.

## Reading it

The session screen, or `recall-cli sessions` and
`recall-cli transcript meeting-20260209-1033`. Long recordings come out rough
(mis-assigned turns, run-on blocks); correct in the app, then export, which is
deterministic. Quality: 28% WER on the AMI meeting ES2004a (#1470).
