# Capture startup dead-window

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
