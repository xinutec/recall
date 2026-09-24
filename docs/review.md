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

Columns: session id, when, duration, turn count, and the confirmed speakers (or
`unknown` where none have been named yet).

## Read one session

```
recall-cli transcript meeting-20260209-1033
```

```
# meeting-20260209-1033  (Mon 09 Feb 2026 10:33)

[10:33:03] Alex: Thanks for fitting me in this morning.
[10:35:16] Dr. Adams: Of course — let's go through the results together.
```

Each line is `[time] speaker: text`. The speaker is a **confirmed name** where one has
been entered; otherwise the **diarization voice** (`SPEAKER_00`, `SPEAKER_01`, …), so
distinct unnamed speakers stay distinguishable; `unknown` only if there's neither.

## Read a day's calls (continuous capture)

Phone calls and in-person conversations caught by the always-on mics aren't "sessions"
— they're split out of the day's continuous recording by silence gaps. List a day's
conversations, then dump one by its number:

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

Times are local. `--day` takes `today` or `YYYY-MM-DD`; `--conv N` (or `--conv last`)
dumps conversation N
(same `[time] speaker: text` format). The redundant room mics are folded to one line
per moment, so the dump isn't doubled.

## Machine-readable

`recall-cli` prints for a reader. For structured output call the API directly:
`GET /api/sessions/<id>/transcript` is the session's clean export, one bubble per
run of same-speaker turns, current state only, deterministic. It sits behind the
Nextcloud sign-in like every browsing route; `cli/src/api.rs` shows how the CLI
carries the cookie.

## Editing in the app

Fixing is in the web UI. The timeline (`/`) and a session (`/sessions/<id>`) show
turns the same way: paragraphs per speaker, a guess in italics with its strength,
grey lines still being processed. Every edit supersedes the old turn; nothing is
deleted, and a later pass never overwrites it.

- **Who said a line**: tap it, then a name (or type a new one). This files a
  correction, so it also enrols the voice.
- **Move a phrase**: drag across the words someone else said and pick the speaker.
  The cut snaps to word boundaries. A selection across two mics is refused.
- **Fix the words**: tap the line, then *Fix words*.
- **Other mics**: a small number after a line counts the mics that heard it; tap
  the line, then *Other mics* to read and hear their versions. A wavy underline
  means the mics disagree on who spoke.
- **Grey lines** are editable once they settle; an edit before then would be
  replaced by the next pass.

## What to trust (and what not to)

- The **text** is automatic speech recognition (Whisper). It mishears — especially
  names, drug names, and medical terms. Don't quote a single word as fact.
- **Speaker attribution** comes from diarization + voiceprints + human review. A
  **confirmed name** is reliable. A bare `SPEAKER_nn` only means "a distinct voice" —
  it is *not* verified to be one person throughout, and the diarization can mis-sort an
  individual turn. Treat any unconfirmed attribution as a hint, not a fact.
- Corrections are made in the web UI; this command reflects the corrected state.
