# Reviewing a recorded call

⚠ **`recall-cli` reads the FLEET**, which is the archive of record; the Python
`recall.sh transcript` these examples used to show was deleted with the CLI
(#1342) and read a Mac database that has been frozen since July.

To read a recorded session (a meeting / phone call) from the command line — for your
own review, or to hand to another agent — use the `transcript` command. It reads
straight from the store; no server needed.

The data lives at `/Volumes/Backup/recall`, which is already the default on this Mac
(the data root) — pass `--out` only to point somewhere else.

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

Reading is on the command line; fixing is in the web UI — the **timeline** (`/`, the
day-by-day continuous capture) and the **session view** (`/sessions/<id>`, an uploaded
meeting). Both share the same editing set; every edit is **versioned** (it supersedes the
old turn, nothing is deleted) and a re-derivation pass never overwrites it.

- **Reassign a whole turn** — tap the speaker chip and pick a name (or type a new one).
- **Split out a phrase** — drag-select the words someone else said; an assign bar opens,
  tap or name the speaker, and that phrase is carved into its own turn. The cut snaps to
  word boundaries and plays audio-exact where the turn has word timings; on older turns
  without them it's a character estimate you then fine-tune by ear. A selection that
  crosses two recordings (e.g. two mics) is refused — a split belongs to one source.
- **Trim a boundary** — ⋮ → *Trim audio*, then nudge the start/end and replay until the
  clip holds exactly the words. The timeline has a coarse 0.5s step and a fine 0.1s one,
  and can pull a start earlier into a gap, down to the segment start.
- **Coalescing** — consecutive turns by the *same confirmed speaker* read as one block:
  the name shows once and continuations carry it dim (still tappable to re-tag). Only
  confirmed speakers coalesce; unknown turns never do (two adjacent unknowns aren't
  necessarily the same person).
- **Refine this section** — an expanded timeline conversation has a *Refine this section*
  action that queues an on-demand diarize-refine of that stretch. It's processed by the
  idle-gated refine daemon (so the heavy pass stays off live capture), which re-derives
  those segments — better transcription + re-split speakers — superseding the machine
  turns; your corrections are untouched.

## What to trust (and what not to)

- The **text** is automatic speech recognition (Whisper). It mishears — especially
  names, drug names, and medical terms. Don't quote a single word as fact.
- **Speaker attribution** comes from diarization + voiceprints + human review. A
  **confirmed name** is reliable. A bare `SPEAKER_nn` only means "a distinct voice" —
  it is *not* verified to be one person throughout, and the diarization can mis-sort an
  individual turn. Treat any unconfirmed attribution as a hint, not a fact.
- Corrections (names and text) are made in the web UI at `/sessions/<id>`; this command
  reflects the current corrected state.
