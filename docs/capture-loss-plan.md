# Plan: why speech isn't recorded correctly, and how to fix it

Scope: the **short flow** — resume capture, say a few words, pause — must reliably record
the words. Tonight (2026-07-15) it did not: spoken upstairs into the Pixel 9, the test
phrase landed in **no** transcript on any device. This plan is how we find out *why*,
then fix it. It continues [capture-startup-deadwindow.md](capture-startup-deadwindow.md)
(the root investigation) and the loss-visibility work already shipped.

**Governing principle: diagnose before fixing.** The dead-window has been misattributed
repeatedly (sox vs ffmpeg, multi-client contention, faint-audio confounds). We do not add
a fix until instrumentation has *shown* which mechanism fired. Every fix below is
contingent on Phase 1–2 confirming its cause.

## What we saw tonight (facts)

- Capture was on for ~25 s (resume 21:04:42Z → user re-paused 21:05:07Z).
- Only `iphone11` wrote a segment: −60 dB max = faint room noise, no speech (correct — the
  user was upstairs, iphone11 downstairs).
- `pixel9` (spoken into), `pixel5`, `usb` wrote **no** real segment; `capture_events` show
  empty `dead_window` stubs for all three at resume.
- Phrase (`mechanical man mincing meat…`) is in no `transcript_segments` row.
- The app's Devices dots were green ("active") during the window — but "active" means
  *connected*, not *recording real audio*, so the user spoke into a not-yet-recording gap.

## What we do NOT yet know (must be measured, not guessed)

1. Did the Pixel 9 stream **digital silence** (its documented phone-side bug) or produce an
   empty **startup** segment like the others? Spoken *into*, an empty segment points at
   silence — but we have not measured its actual sample level.
2. Did the speech-bearing samples ever **reach the ingest** and get discarded, or were they
   never captured at all?
3. Does **pausing mid-segment** finalize the in-progress audio, or drop it? A ~25 s window
   is cut by the pause; if the flush discards the partial segment, that alone loses it.
4. How long is the **startup dead-window** per source, and does the first-segment-after-
   resume reliably come up empty?

The evidence that would answer these is currently **destroyed**: `worker._clear_dead_stubs`
unlinks the zero-byte segments before we can inspect them. That is the first thing to fix.

## Phase 1 — instrument so the failure is observable — DONE 2026-07-16

No behaviour change to capture; only make it *legible*. Small, safe, shipped first.

1. **Stop destroying evidence.** ~~Record the stub's byte size / sample count / dB before
   unlinking, quarantine under a flag.~~ Resolved by reading the code: a dead-window stub is
   **zero bytes by construction** (`probe.Scan.empty` is exactly the `st_size == 0` files),
   so there is nothing to measure or quarantine — a stub *cannot* hold silence. "Silent vs
   empty" is therefore settled on the ingest side (item 2): ffmpeg receiving 25 s of digital
   silence writes a small-but-nonzero FLAC/Opus segment; a zero-byte file means ffmpeg
   received (almost) nothing. And `recall capture-trace` (item 3) shows fresh stubs live,
   from disk, before the worker clears them.
2. **Per-source ingest telemetry.** `stream_server` now meters every connection as it pumps
   (`StreamMeter`): on close it writes a durable `ingest_disconnect` capture event with
   seconds connected, bytes received, peak dBFS, time-to-first-byte, time-to-first-*audible*-
   sample (≥ −66 dBFS — above the pixel9 −90 dB silence signature, below any live room), and
   which segment file the close flushed with how many bytes. `ingest_connect` marks the open.
   So a resume answers "pixel9 connected at T, sent N bytes at −90 dB" (silence) vs "−25 dB"
   (real audio) — directly settling question 1.
3. **Resume timeline.** `recall capture-trace [--minutes N]` prints one merged, time-ordered
   trace: mirror intent applications (new durable `mirror_applied` event + timestamped agent
   logs), USB resume/pause events, phone connects/disconnects with their measured levels,
   dead windows, indexed segments, and fresh **un-indexed** segment files straight from disk
   (the min-age guard means the store lags a live sitting by minutes). This makes the gap
   between "resumed" and "actually recording" measurable per source.
4. **Pause-flush trace.** The `ingest_disconnect` event's `flushed`/`flushed_bytes` fields
   say what the close finalised (phones). The USB path's flush is visible as the segment
   file itself (trace item 3); its finalisation on pause is already handled by
   `runner._run_pipe` (producer closed first, ffmpeg finalises on EOF with a 10 s grace).
   Settles question 3.

Not measurable inline: the USB mic's first-audible time — its PCM flows producer→ffmpeg
through a kernel pipe with no Python in the path, deliberately. Its levels come from
decoding its segments after the fact, which the archive already preserves.

Deliverable (met): after Phase 1, one controlled resume produces a readable trace
(`recall capture-trace`) that says which source recorded what, at what level, and when.

## Phase 2 — controlled diagnosis (needs a person at the mic)

Capture must never be resumed unprompted; these run with the user speaking. Use the
acoustic-loopback method already proven (a distinctive phrase; verify by transcript, not by
ear). Separate the variables the 25 s test confounded:

1. **Long window.** Resume, wait for real audio confirmed landing (Phase 1 trace), speak,
   then keep recording ~2 min before pausing. Does each source *ever* capture the speech in a
   stable (non-startup) segment? This splits "startup dead-window in a short window" from
   "this device never captures."
2. **Per-device isolation.** Speak *into* the Pixel 9 as the sole nearby source; repeat for
   Pixel 5 and the USB mic. Confirms per-device whether the mic yields real samples
   (question 1) — in particular whether the Pixel 9 silence bug reproduces when spoken
   directly into.
3. **Short window, instrumented.** Reproduce tonight's 25 s flow with Phase 1 tracing on, to
   see exactly which hop dropped the speech and whether the pause discarded a partial segment.

Outcome: a confirmed mechanism (or mechanisms) for each source, written back into
[capture-startup-deadwindow.md](capture-startup-deadwindow.md).

## Phase 3 — fixes (each contingent on Phase 2 confirming its cause)

- **Startup dead-window (first N s not recorded).** Warm-up-and-verify: open the device on
  resume and do not report "recording" until non-silent samples are confirmed landing; if a
  producer yields only zero-level input for N s, cycle it (a coreaudio re-open clears a
  stall). Gate on *signal present*, not loudness, so a genuinely quiet room isn't cycled.
- **"Active" means recording, not connected.** Make `/api/sources` `active` (and the app's
  dot) reflect real audio landing recently — a non-empty segment / level above the source's
  floor within the window — not just a fresh `.alive`. This is the cheapest fix that would
  have *prevented tonight's loss*: the user would not have spoken into a dead window. Both a
  UX fix and a safety fix.
- **Pixel 9 silence.** If confirmed phone-side: an `AudioRecord` silence watchdog on Android
  that restarts the record path when it yields sustained zero-level (the iOS app already has
  this — `Watchdog`/`StreamClient`; port the idea). Needs on-phone `adb` confirmation first.
- **Pause-flush.** If the pause drops a partial segment: finalize the in-progress segment with
  its audio on pause, so words spoken just before a pause are never lost.

## Phase 4 — regression guard (so this can't silently come back)

1. **Automated acoustic-loopback assertion.** A committed script: resume → play a known
   phrase through a speaker → pause → wait → assert the phrase transcribes on the target
   source. Run it after any capture-path change. Makes "the short flow records" a testable,
   non-regressing property rather than a thing we re-discover by hand.
2. **Confirm `recall doctor` would have caught it.** The Track-A speech-loss alarm exists;
   verify tonight's pattern (unexplained gap during an active span) actually trips it, and
   tighten if not.

## Order and cost

Phase 1 first (small, safe, no behaviour change) — without it we keep guessing. Then Phase 2
with the user (one sitting). Phase 3 fixes only the confirmed causes. Phase 4 locks it in.
The "active means recording" fix (Phase 3) is worth pulling early: it's cheap and it directly
stops a user speaking into a dead window, which is the specific way tonight's words were lost.

Not in scope: the Mac↔Isis poll cadence — deliberately left at 5 s; it affects resume
*latency*, not whether captured audio is recorded correctly, which is the actual failure here.
