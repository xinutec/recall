# Reviewing a recorded call

To read a recorded session (a meeting / phone call) from the command line — for your
own review, or to hand to another agent — use the `transcript` command. It reads
straight from the store; no server needed.

The data lives at `/Volumes/Backup/recall`, so every command takes
`--out /Volumes/Backup/recall`.

## List the recorded sessions

```
./scripts/recall.sh transcript --out /Volumes/Backup/recall
```

```
meeting-20260209-1033  Mon 09 Feb 2026 10:33   18m29s    50 turns  Dr. Adams,Alex
meeting-20260202-1529  Mon 02 Feb 2026 15:29    1h07m   275 turns  unknown
```

Columns: session id, when, duration, turn count, and the confirmed speakers (or
`unknown` where none have been named yet).

## Read one session

```
./scripts/recall.sh transcript meeting-20260209-1033 --out /Volumes/Backup/recall
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
./scripts/recall.sh transcript --day today --out /Volumes/Backup/recall
./scripts/recall.sh transcript --day 2026-02-09 --conv 3 --out /Volumes/Backup/recall
```

```
# today — 3 conversation(s)

1. 12:27-12:29   29 turns  Yes, this is the delivery driver calling.
2. 13:41-13:43    7 turns  Could you call me back this afternoon?
3. 16:33-16:40   56 turns  Hello, is anyone home?
```

Times are local. `--day` takes `today` or `YYYY-MM-DD`; `--conv N` dumps conversation N
(same `[time] speaker: text` format). The redundant room mics are folded to one line
per moment, so the dump isn't doubled.

## Machine-readable

Add `--json` to either form for structured output — a session array, or the clean
transcript below — for an agent to parse instead of scraping the text.

## Export a transcript

`scripts/recall-api.py` is a **dependency-free** (stdlib-only) client for a running recall
server — copy it anywhere with `python3`, no install. It defaults to
`http://localhost:8000` (override with `--base-url` or `RECALL_API_URL`) and is read-only
(it never changes capture or any data).

```
scripts/recall-api.py sessions                          # list sessions (JSON)
scripts/recall-api.py transcript <id>                   # the clean export (JSON)
scripts/recall-api.py transcript <id> --markdown        # ready-to-splice md bubbles
scripts/recall-api.py search "<query>" --limit 20
scripts/recall-api.py status | capture | sources | speakers
scripts/recall-api.py timeline [--limit N] [--before <iso>]
scripts/recall-api.py around <turn-id> [-n N]
```

`transcript <id>` returns the session's clean, finalised transcript as JSON:

```jsonc
{
  "session": "meeting-20260209-1033",
  "date": "2026-02-09T10:33:03+00:00",   // first bubble's start (local offset); null if empty
  "speakers": ["Alex", "Dr. Adams"],     // confirmed names present, in first-seen order
  "turns": [
    { "start": "2026-02-09T10:33:03+00:00", "speaker": "Alex",
      "text": "Thanks for fitting me in this morning." },
    { "start": "2026-02-09T10:35:16+00:00", "speaker": "Dr. Adams",
      "text": "Of course — let's go through the results together." }
  ]
}
```

- **Coalesced**: consecutive same-speaker turns are merged into one bubble. `speaker` is
  a confirmed name, or `SPEAKER_nn`/`unknown` if not yet confirmed (see trust note below).
- **Current state only**: superseded/hidden turns and the per-mic alternates are excluded
  — just the finalised, human-corrected reading.
- **Deterministic**: stable order, no generation timestamp. Re-running with no new
  corrections returns byte-identical output (`transcript --json`, above, emits the same).

`--markdown` renders the transcript as `**[HH:MM] Speaker:** text` bubbles. To update
**only** the transcript and leave the rest of a page intact, wrap the block in markers and
replace between them on each export — the bubbles carry no markers themselves:

```markdown
<!-- recall:transcript meeting-20260209-1033 -->
…rendered bubbles…
<!-- /recall:transcript -->
```

Because the export is deterministic, a re-run with no new corrections produces no diff —
the manually-maintained parts of the page are never touched.

> The server runs on the capture host; a remote caller needs a path to it (a tunnel/VPN,
> or run the tool on the host).

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
  turns; your corrections are untouched. To re-derive with a fine-tuned adapter, the
  refine daemon must be pointed at it (`--model <adapter> --base-model …`).

## What to trust (and what not to)

- The **text** is automatic speech recognition (Whisper). It mishears — especially
  names, drug names, and medical terms. Don't quote a single word as fact.
- **Speaker attribution** comes from diarization + voiceprints + human review. A
  **confirmed name** is reliable. A bare `SPEAKER_nn` only means "a distinct voice" —
  it is *not* verified to be one person throughout, and the diarization can mis-sort an
  individual turn. Treat any unconfirmed attribution as a hint, not a fact.
- Corrections (names and text) are made in the web UI at `/sessions/<id>`; this command
  reflects the current corrected state.
