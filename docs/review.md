# Reviewing a recorded call

`recall-cli` reads the fleet, the archive of record, over the same API the web
app uses; reading transcripts needs a browsing session (`recall-cli --help`
says how to sign in). `--api` points it elsewhere.

## List the recorded sessions

```
recall-cli sessions
```

```
meeting-20260209-1033  Mon 09 Feb 2026 10:33   18m29s    50 turns  Dr. Adams,Alex
meeting-20260202-1529  Mon 02 Feb 2026 15:29    1h07m   275 turns  unknown
```

The last column is the confirmed speakers, or `unknown`.

## Read one session

```
recall-cli transcript meeting-20260209-1033
```

```
# meeting-20260209-1033  (Mon 09 Feb 2026 10:33)

[10:33:03] Alex: Thanks for fitting me in this morning.
[10:35:16] Dr. Adams: Of course — let's go through the results together.
```

The speaker is a confirmed name, else the diarization voice (`SPEAKER_00`, ...),
else `unknown`.

## Read a day's calls (continuous capture)

Conversations caught by the always-on mics are not sessions; they are split out
of the day by silence gaps:

```
recall-cli day 2026-02-09
recall-cli day 2026-02-09 --conv 3
```

```
# today — 3 conversation(s)

1. 12:27-12:29   29 turns  Yes, this is the delivery driver calling.
2. 13:41-13:43    7 turns  Could you call me back this afternoon?
3. 16:33-16:40   56 turns  Hello, is anyone home?
```

Times are local. The date is `YYYY-MM-DD`, `today` or `yesterday`; `--conv N`
(or `last`) reads one conversation. Mics that heard the same moment are folded
to one line.

## Machine-readable

`GET /api/sessions/<id>/transcript` is the session's export: one bubble per run
of same-speaker turns, current state, deterministic. It needs the sign-in
cookie, as in `cli/src/api.rs`.

## Editing in the app

Fixing is in the web UI. The timeline (`/`) and a session (`/sessions/<id>`) show
turns the same way: paragraphs per speaker, a guess in italics with its strength,
grey lines not yet speaker-separated. Every edit supersedes the old turn; nothing is
deleted, and a later pass never overwrites it.

Tapping a line opens its sheet at the bottom of the screen:

- **Who said it**: tap a name, or type one under *Someone else*. This files a
  correction, so it also enrols the voice.
- **Part of it was someone else**: tap the first and the last word they said, then
  the name. A phrase across two lines takes one split per line.
- **Fix words**, **Copy words**, and a link to the line on its own.
- **Other mics**: a small number after a line counts the mics that heard it; the
  sheet lists their versions to read and hear. A wavy underline means the mics
  disagree on who spoke.
- **Grey lines** have no speakers separated yet. They are editable; every pass
  keeps a correction. Only a live line, minutes old, waits for its transcription.

## Checking words

*Check* (`/check`) goes through a day one line at a time. It asks about the
words only; who spoke is named in a session or on the timeline. Each line plays by
itself; fix the words, or tap *Words are right*. Enter saves, *Skip* is for a
line you can't make out. Where several mics heard a moment, the page takes them
in turn, because a check scores the mic whose text was edited. A line plays
with a second either side, since Whisper often ends a line before its last
word. Below it, *Other mics heard* lists the same moment on the other mics;
tap one to start from its words, which helps with names.

*Nobody spoke* is for words the model invented over silence. It hides the line
and files the correction with empty text, so the span stays protected and a
later pass cannot write the same words back. The line sheet on a session or the
timeline has it too, and both offer *Undo* for a mis-tap, which shows the line
again and drops the correction.

Every answer can be taken back: the message after it offers *Undo*, and a line
already checked shows *Undo check* when you go *Back* to it. Undo deletes the
correction rather than hiding it, since a mis-tap was never a judgement.

The same three answers are in the line sheet on a session or the timeline, and
they are stored the same way wherever given: each files a correction marked
`words_checked` (a person heard the words and vouches for them), and the person's
turn carries `words_checked` too. Check skips a moment once any of its lines has
it, so a day can be worked in a session first and finished in Check. A speaker
fix keeps the machine's words: it leaves the line to be checked, and a referee
scoring words against corrections must not count it.

## What to trust

- Unchecked text is Whisper's and mishears names and medical terms: do not quote
  a single word as fact.
- A confirmed name is reliable. `SPEAKER_nn` means a distinct voice, not
  verified to be one person throughout; an unconfirmed attribution is a hint.
