# Capture dead-window

> **RESOLVED 2026-07-15 — root cause was `sox`.** recall read the mic through sox's
> CoreAudio driver, which intermittently wedges: its input silently drops to digital
> zero for minutes at a time (segments come out zero-byte, cleared as dead stubs) while
> the device stays perfectly healthy. Proven side-by-side — `ffmpeg -f avfoundation`
> read real audio (−57 dB) from the same USB mic *at the instant* sox was writing empty
> segments; the device enumerated fine throughout; and it happened with recall-live
> stopped (so not two-client contention) and mid-session (so not a startup warm-up).
> **Fix (commit `51948fa`): read the mic via ffmpeg avfoundation, not sox**
> (`sources.producer_argv`, CoreAudio case — used by both recall-capture and
> recall-live). Validated end-to-end with `runner.record` (a real non-empty segment).
> The history below is the investigation that ruled out the wrong causes.
>
> **Confirmed again 2026-07-15 via acoustic loopback (no human, capture left paused).**
> Three known TTS phrases played through the Mac mini Speakers were read back off the
> USB mic with an *isolated bounded* `ffmpeg -f avfoundation` to a scratch WAV (not
> production capture; the pause file was untouched). The window carried real audio
> (max −19.6 dB / mean −44.2 dB, vs ~−91 dB for the sox dead-window) and mlx-whisper
> turbo recovered all three phrases verbatim. The ffmpeg path read real intelligible
> room audio and did not wedge over the window. Reusable technique: play known audio
> through a speaker + read the mic in isolation to verify the capture/ASR path with no
> person at the mic and without resuming the always-on recorder.

Every capture session can begin with a run of **zero-byte segments** — capture is
running and rolls a fresh file each minute, but the mic delivers only digital
silence, so the opening of the session records nothing. The worker clears the empty
stubs as dead capture (`worker._clear_dead_stubs`), leaving only a per-file log line
and a gap in the archive. This is silent loss of the start of a recording, and it
recurs.

## Evidence

Two mechanisms, both observed on 2026-07-14. Numbers are from the archive DB and the
`logs/capture.err.log` / `logs/worker.err.log` on the Mac.

**The dead-window itself.** `logs/worker.err.log` records the cleared empties; their
timestamps begin at the exact second of each `recall.capture: listening:` event and
continue at 60 s intervals. The length varies per session:

| session start (`listening`) | empty minutes cleared | so audio began |
|---|---|---|
| 19:36:29Z | 19:36 → 19:48 (**13**) | ~19:49 |
| 21:20:04Z | 21:20 (**1**) | ~21:21 |
| 22:04:14Z | 22:04, 22:05 (**2**) | ~22:06 |

All four mics (usb, pixel5, pixel9, iphone11) go empty together at each start, so it
is not one bad device. During the window sox logs `In:0.00%` (input level zero) and
`coreaudio: unhandled buffer overrun. Data discarded` — the device is delivering
silence, not real audio that is being dropped downstream.

**It loses real speech, not just quiet.** At 22:04 the user counted 1→90 aloud, then
paused 30 s, then counted 1→60. Only the second count reached disk (segment 9536+,
from 22:06:14). The first count fell entirely inside the 2-minute dead-window and was
never recorded. Notably the *live* pipeline — a second `sox` stream on the same
coreaudio device (`live.mic_argv`) — did produce transcripts for that first count, so
one sox client caught audio while capture's client got silence at the same instant.

## Mechanism (inferred)

Capture and live each open the USB coreaudio device with their own `sox` process
(`sources.producer_argv` / `live.mic_argv`). The zero-input-level + buffer-overrun
signature, aligned exactly to capture start and lasting a variable 1–13 minutes,
points to a **coreaudio multi-client startup issue**: the capture `sox` client reads
the device but receives digital silence for a warm-up period before real samples
flow. Encoded as Opus (voip/DTX) a pure-silence minute muxes to a zero-byte segment,
which the worker then clears as a dead stub. This is a hypothesis from the logs; it is
not yet confirmed against the live device (see the test plan).

## 2026-07-15 controlled test — what it ruled out

Resume-from-pause at 23:30:25Z, capture ran to 23:34:33Z, user counted 1→30 from bed
(far from the USB mic, pixel9 in hand). Result: the count is **nowhere** — no clean
audio, no live turns. What the test established:

- **Multi-client contention is NOT the live crash.** `recall-live`'s `sox` aborts
  (`buffer overrun → Aborted`) even with capture *paused*, i.e. as the sole client on
  the device — so two-sox-on-one-device is not why live dies. Live has produced nothing
  since 22:07Z.
- **The device and coreaudio are healthy.** `system_profiler` shows the USB Condenser
  Microphone present, active, 48 kHz, default input; `coreaudiod` uptime 3d+ (not
  wedged — a wedge shows a fresh restart). So the abort is not a dead device or daemon.
- **The USB dead-window test was inconclusive this round.** The user was far away and
  mostly silent, so the empty USB segments are confounded with "quiet far room + Opus
  DTX". The strong dead-window evidence is still the 22:04 *loud*-count test.
- **The pixel9 streamed digital silence, not a dead-window.** It connected at 23:30:29Z
  and stayed connected, but every segment is ≈ −90 dB (digital zero) — the phone was not
  capturing mic audio. NOT backgrounding: the mic app was foreground for ~2 of those
  minutes. Cause unknown — needs on-phone diagnosis (`adb logcat` of `org.recall.mic`,
  mic appops/permission, whether `AudioRecord` yields samples) while counting.

**Live-crash hypothesis (unconfirmed).** The read loop runs Silero VAD inline per chunk
and offloads whisper to a thread, so under CPU load the loop can fall behind realtime,
fail to drain sox's stdout, and overrun the coreaudio ring buffer → `sox` aborts. Fix
would be a dedicated drain thread that empties sox's stdout into a buffer regardless of
VAD/whisper timing (the docstring already names this failure mode). Not yet built — the
mechanism is inferred, not reproduced on demand, and the live path is currently untested.

## 2026-07-15 morning — live overrun fixed, dead-window seen MID-session

Resume 08:57:05Z, user counted loud+close, then a test sentence at 09:18 before pausing.

- **`recall-live` overrun is FIXED.** `live.drain_to_queue` runs a dedicated thread that
  empties sox's stdout regardless of VAD/whisper timing (commit `3d98063`). Overruns went
  from 601+ in minutes to **0** after deploy; capture's ffmpeg consumer on the same mic
  never overran, which is what fingered the slow Python reader.
- **The dead-window is NOT only at startup.** Capture wrote real audio 08:57–09:02 (four
  ~187 KB segments) then **16 empty segments 09:02→09:18** (device went to digital silence
  mid-session). The 09:18 test sentence landed in it — empty capture, one garbled live
  fragment ("Rame."). So a running session can silently go dead for many minutes.
- **New hypothesis:** `recall-live` crash-looping on the OLD code (each abort → launchd
  relaunch → reopen the *shared* USB coreaudio device) may knock capture's stream into
  silence. The drain fix stops that thrashing, so future sessions MAY see fewer
  dead-windows — unproven (this session's window was already stuck when the fix deployed
  ~09:08 and persisted to pause). Needs a clean session to confirm.
- **Candidate real fix (mid-session too):** a capture silence-watchdog — while unpaused,
  if segments go empty (device delivering zero) restart capture's producer to re-open the
  device. Recovers regardless of root cause. Testable; needs a person + running capture.

## Open faults after the test

1. **Capture startup dead-window** (original) — root cause still open; the confound above
   means the next confirming test needs the user *loud and close* to the USB mic.
2. **`recall-live` aborts** — down since 22:07Z; likely the inline-VAD reader overrunning
   coreaudio under load; drain-thread fix designed but unbuilt/unverified.
3. **pixel9 streams silence** — phone-side capture failure, needs `adb` diagnosis.

## What is in place

- **Live transcripts in a never-recorded gap are no longer hidden.** Reconcile used to
  hide every live turn before the latest archive transcript (a watermark); it now hides
  a live turn only where a transcribed audio segment actually spans it
  (`store.hide_provisional_covered`, `worker.reconcile_live`). So when the dead-window
  swallows the clean audio, the rough live transcript of that moment survives as the
  record instead of vanishing. `store.restore_uncovered_provisional` un-hid the 89
  turns the old rule had wrongly hidden (round-1 count + the walking-test opening).
- **The health check no longer reads a dead-window as healthy.** `recorders_on_disk`
  skips zero-byte segments, so a sustained dead-window (empties only, no real audio for
  > `SILENT_AFTER` = 5 min) surfaces the always-on mic as `fail` via `recall doctor` /
  fleetwatch instead of passing on the fresh-but-empty files.

Neither of these stops the loss — they make it survivable (live record kept) and
visible (health fails). The root cause is still open.

## Open: stop the loss at the source

The fix has to keep capture from silently recording silence at the start of a session.
Options, in rough order of preference:

1. **Warm-up-and-verify the producer.** Before logging `listening`, confirm the device
   is delivering non-silent samples (or at least *some* signal); if a started producer
   yields only zero-level input for N seconds, tear it down and re-open the device. A
   coreaudio re-open often clears a multi-client stall. Risk: distinguishing a real
   silent room from a dead stream — gate on device signal, not loudness.
2. **One device reader, teed.** Have a single `sox`/coreaudio reader feed both capture
   and live (a tee), removing the two-clients-one-device contention entirely. Larger
   change; also removes a class of future races.
3. **Restart-on-empty watchdog.** Independently of the above, if the worker sees a run
   of empty stubs from an always-on mic while unpaused, signal capture to cycle its
   producer.

### Test plan (needs a person at the mic — capture must never be resumed unprompted)

1. With capture paused, resume it and immediately speak a known phrase / count for
   ~3 minutes without pause.
2. From `logs/capture.err.log` note the `listening:` second; from the `usb/` directory
   check how many leading segments are zero-byte (`find usb -size 0`).
3. Correlate against `logs/worker.err.log` "removed empty file" lines and the live
   turns for that window: confirm the leading minutes are empty for capture while live
   still transcribes.
4. Repeat a few times to characterise the window-length distribution and whether it
   correlates with how long the device was idle before the session.
5. Try fix (1): re-open the producer on zero-level input and re-run steps 1–3 to see the
   leading empties disappear.
