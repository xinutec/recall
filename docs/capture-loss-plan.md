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

## Phase 2 — controlled diagnosis — RUN 2026-07-16 (autonomous, speaker loopback)

Run with per-session authorization while the house was empty, using the Bose +
`scripts/loopback-test.py` (the committed Phase-4 harness) and a live segment-size watch.
Confirmed mechanisms:

1. **The USB mic was losing ~22% of ALL samples, continuously** — the 2026-07-15
   sox→ffmpeg-avfoundation switch reintroduced avfoundation's known drop (measured: 15 s
   wall → 11.6 s audio; same ratio at 30 s; `-thread_queue_size` no help; in-agent windows
   as bad as 15.6 s audio per 190 s). Real windows since the switch: 1–15 s of audio per
   minute. sox re-measured sample-perfect (15.000 s in 15 s). **This — not a startup
   dead-window — is why usb dead-windowed every resume since the switch.**
2. **Segment files get their bytes only when the segment CLOSES** (rotation or EOF) —
   watched live: the current segment sits at 0 bytes, held open by ffmpeg (lsof), for
   minutes. So a 0-byte *newest* file is normal, not evidence of death — and:
3. **The worker could delete ffmpeg's open segment.** A stalled producer keeps one segment
   open past the 120 s dead-stub bar; `_clear_dead_stubs` unlinked it while ffmpeg held the
   fd, sending the eventual flush to a deleted inode — silent, unrecoverable loss (this is
   the likely shape of several past "dead windows", including the 12:14 window's whole usb
   audio today).
4. **pixel5 "failures" in loopback are physics** — it streams fine (bytes + room floor
   −51 to −69 dB) but sits too far from the Bose to hear a phrase above −40 dB.
5. **Phone reconnect latency after a resume is variable: 3 s to 62 s** (OS-throttled
   background retry timers; both phones connected at the same instant in the slow case).
   The 21:04 loss happened partly because the phones weren't connected yet when the phrase
   was spoken — and "active" dots said they were.
6. **iphone11 end-to-end is solid** (phrase at −13.5 dB, transcribed). The pixel9's
   zero-byte close (nothing flushed despite the pause finalising the segment) means it sent
   ~no PCM in that window — phone-side, reproducible only with the pixel9 present; not a
   priority.

Question 3 (pause-flush) is answered: the pause path DOES finalise and flush the open
segment (usb's 27.6 s of a 28 s window arrived at close). Nothing is dropped by pausing.

## Phase 3 — fixes — SHIPPED 2026-07-16 (each cause measured first)

- **sox restored as the USB producer** (sample-perfect; avfoundation cut). The live tap
  moved to the segmenter (`build_segment_argv(..., fanout=True)`) since sox has one output.
- **Dead-segment watchdog** (`runner._watch_dead_segments`): two consecutive closed
  segments of digital silence (a sox wedge keeps rotating silent segments), or rotation
  stalled ≥3 segment-lengths (a producer delivering nothing), terminate the producer; the
  agent respawn re-opens the device (which clears a CoreAudio wedge); durable
  `producer_cycled` event. This is what makes sox's rare wedge cost minutes, not a
  recording.
- **The worker never deletes a source's newest (possibly open) zero-byte segment** —
  closes mechanism 3.
- Validated live: full resume→phrase→pause loopback PASS — usb −22 dB (27.6 s/28 s
  window), iphone11 −13.5 dB, phones connected in 4 s.

Still open (deliberately):
- **"Active" means recording, not connected** — unchanged from the original plan; now
  backed by finding 5. The phones' meter data (`ingest_disconnect` stats / a live meter
  read) is the honest signal. Next piece of work.
- Phone reconnect latency (finding 5) — mitigated by honest "active" dots; a push-style
  reconnect nudge is possible but adds machinery.

## Phase 4 — regression guard — IN PLACE 2026-07-16

1. **`scripts/loopback-test.py`** (committed): resume via the fleet (the real production
   path) → play a nonce phrase through a speaker → re-pause (restore is in a `finally`) →
   judge per source from the ingest telemetry (phones) or the segment files (usb), with an
   optional transcript tier. Run it after any capture-path change:
   `scripts/loopback-test.py --speaker "Bose Revolve SoundLink" --expect usb --expect iphone11`
   (pixel5 can't hear the Bose from its room — expect it only with a nearer speaker).
   Note: `say` blocks for up to ~a minute while a sleeping Bluetooth speaker wakes; the
   script's timestamped lines make that visible.
2. **Loss reconciler upgraded to coverage** (`loss.uncovered_loss`): any uncovered part of
   an active span ≥2 min is loss — a span with NO segments at all (the crash-loop shape)
   is caught now, which gap-between-segments missed. `recall doctor` also counts
   dead-window events (it flags today's, correctly).

## Order and cost

Phase 1 first (small, safe, no behaviour change) — without it we keep guessing. Then Phase 2
(ran autonomously via speaker loopback with per-session authorization). Phase 3 fixed only
measured causes. Phase 4 locks it in. Still open: the "active means recording" surface (see
Phase 3) — the remaining piece that stops a person speaking into a not-yet-recording window.

Not in scope: the Mac↔Isis poll cadence — deliberately left at 5 s; it affects resume
*latency*, not whether captured audio is recorded correctly, which is the actual failure here.
