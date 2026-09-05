# The audio plane — audiod

`audiod/` is the Rust daemon that owns the audio plane: everything from
transducer to filesystem. The Python half owns everything from filesystem to
meaning (ASR, diarization, speaker ID, the LLM) — that split is deliberate and
stays (see [design.md §9](design.md)). The two halves meet only at:

- segment files named `<source>-<UTC-start>.<ext>` under `<root>/<source>/`
- the `.alive` liveness marker (touched only on measured signal)
- the `capture_paused_until` pause file (audiod reads, Python writes)
- two bookkeeping writes into `recall.sqlite` (source registration,
  `capture_events`)

Nothing downstream may be able to tell which language wrote a segment.

## Why a daemon, and why one room stream

Measured 2026-09-04 (tracking issue #1414): the worker is GPU-bound — 93–97%
sustained GPU while nine of ten CPU cores idle — and transcribes each of five
co-located microphones separately, completing 1.6 segments/min against 4.0
arriving. (Seen again live on 2026-09-05 under the Rust plane, with all five
mics up: 75 segment-minutes reached the archive while 9 were transcribed.)
Transcribing the room once instead of five times (#1388) is what recovers that
budget, and it needs a single `room` stream — by choosing between the sources,
by combining them, or both. Whichever wins, it must be decided where all five
exist in phase-intact form, which is only in RAM at ingest: the archive is Opus
32 kbps voip, which does not preserve phase. So the daemon that receives the
raw PCM is the only place this can live.

The streams are not peers (the best mic's noise floor measured ~28 dB below the
worst's), and the phones apply their own AGC/noise suppression, which is
non-linear and time-varying. Two consequences:

- naive averaging would degrade the best microphone; combination must be
  weighted by per-channel, per-band SNR
- coherent (phase-aligned) methods can only be attempted per block, with a
  measured-confidence gate; a stream whose phase is mangled still contributes
  at the magnitude tier

## Alignment: similarity, not clocks

The clock (the handshake's capture epoch) only bootstraps a search window.
Alignment itself comes from the audio, in three tiers, each narrowing the next:

1. **envelope correlation** (~100 ms) — survives AGC, noise suppression and
   Opus; works on decoded archive audio too
2. **onset / spectral-flux correlation** (10–20 ms) — frame accuracy, all that
   magnitude-domain fusion needs
3. **GCC-PHAT** (sub-sample) — only for the coherent tier: beamforming and the
   TDOA position features

Anchors accumulate into a per-device offset+drift model (crystal drift is tens
of ppm, so silence is coasted on the fitted slope). Correlation confidence
gates each source's fusion tier per block; a collapse means the device left
the room and is excluded from the fused output while still being archived as
its own source. A source never falls out of the system — it slides down a
tier.

## The products, in build order

1. **Ingest server** — same wire protocol (`src/recall/wire.py` +
   `audiod/tests/handshakes.json` are the shared-facts references), same
   archive layout, same pause behaviour, same `capture_events` evidence.
   **Deployed 2026-09-04**; the Python server it ported is deleted — the
   rollback is git history. The tier-1 aligner (`align.rs`,
   `envelope.rs`, the `align-probe` bin) is also **built and measured**: on
   the pre-epoch-fix June archive every 60 s block anchors at peak r
   0.66–0.94 with sub-second, smoothly drifting offsets.
2. **USB capture** — `audiod capture`: sox producer, metered pump,
   dead-segment watchdog, pause parking — the port of `recall record`
   (now deleted). **Deployed 2026-09-05** during a capture pause; expect the
   TCC mic prompt at the binary's first device open. Replacing sox itself
   with an in-process CoreAudio read is a later, separate step behind the
   same watchdog.
3. **One `room` source, transcribed once** — the worker transcribes `room`
   first and per-source becomes backfill. The freed GPU budget (one
   transcription, not five) also makes non-turbo `large-v3` — the open
   better-on-Dutch lever in [pipeline.md §2](pipeline.md) — affordable on the
   room stream; the A/B harness to decide that already exists.
   **How `room` is produced is now an open question: SNR-weighted fusion was
   measured and FAILED its gate** (see "What the gate measured" below). The
   capacity win does not depend on fusion — it depends on transcribing one
   stream instead of five, and the best-measured single stream currently beats
   every fusion we have built.
4. **Spatial features** — per-frame TDOA/level vectors across devices as
   sidecar files: a position fingerprint that separates same-voice/different-
   seat where voiceprints confuse, feeding diarization as a third view, and
   the mask source for person-filtered output (GSS-style).

The WER bake-off in (3) did not wait for (2): it runs offline over the
already-recorded archive (magnitude-tier alignment works on decoded Opus), so
the fusion question was answered before the sacred capture path was touched.
It runs at ~90x real-time in a release build, which puts continuous alignment
well inside the ingest budget whatever `room` ends up being.
ffmpeg remains the segmenter child for now; native Opus encoding arrives
with (3), which needs the PCM in process anyway.

## What the gate measured

Run 2026-09-05 over the 2026-06-23 20:12Z window, 38 human corrections with a
covering `usb` segment; both arms get identical spans, one model, no vocabulary
prompt (`scripts/fusion_bakeoff.py`, reports under `/tmp/fusion-bakeoff/`).
Verified first that the arms are comparable at all: each fused clip
cross-correlates with its reference clip at zero lag, peak r 0.92-0.98.

| arm | median WER |
| --- | --- |
| `usb` alone (best single mic) | 0.229 |
| fused, phase from the reference | 0.348 |
| fused, phase from the per-bin SNR argmax | 0.437 |

Fusion is worse, and not by accident: 14 cases worse against 4 better (exact
sign test p = 0.03). **The gate fails.** The denoising precedent held — a
signal that looks better by construction reached the model and hurt it.

Read the median, never the mean. The referee is deterministic on 37 of 38
cases, but one clip hallucinated a loop in one run (WER 111.5, then 1.0 on the
identical audio next run) and dragged the mean from 0.42 to 3.32 by itself.

Two findings worth keeping:

- **Per-bin argmax phase selection is not a valid combiner.** Sources aligned
  to onset accuracy share no phase reference, so choosing each bin's phase
  from whichever source is loudest splices unrelated phases and changes donor
  between neighbouring bins. `fuse::PhaseSource` now names the choice; the
  `Reference` policy is both coherent and better (0.437 -> 0.348).
- **A build diff is a cheap detector for a chaotic discrete decision.** Under
  argmax phase, debug and release builds of the same commit produced audio
  differing across 75% of samples at 15 dB signal-to-difference — a rounding
  difference flipping the argmax. Under reference phase the two builds are
  byte-identical. Any numeric stage whose output moves when only the optimiser
  changed is deciding something discontinuously.

Why fusion loses here is not yet measured, but the candidates are ordered: the
condenser mic's noise floor sits ~28 dB below the phones', so SNR weighting
mixes a much worse signal into an already-good one; the phones' AGC and noise
suppression are non-linear, so their magnitudes do not linearly combine; and a
magnitude-from-many, phase-from-one spectrum is not a consistent STFT, so
overlap-add re-introduces error. The next experiment is therefore not a better
weighting but a narrower question: does fusion ever win a *block*, and which?
If it wins only where two mics are comparable, `room` should fuse there and
carry the best single source everywhere else.

## Deployment notes

- audiod must run as launchd `ProcessType=Interactive` like today's capture
  (design.md §7: `Background` is the throttled class and drops samples).
- Any new capture binary can re-trigger the macOS TCC mic prompt; expect
  `launchctl kickstart -k` after granting.
- The cutover per stage is: run in shadow, compare archives (the loss
  reconciler and the acoustic-loopback test are the referees), then flip the
  agent — and keep the Python path deleted only after its replacement has
  survived real days.
