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

## Why a daemon, and why fusion

Measured 2026-09-04 (tracking issue #1414): the worker is GPU-bound — 93–97%
sustained GPU while nine of ten CPU cores idle — and transcribes each of five
co-located microphones separately, completing 1.6 segments/min against 4.0
arriving. Transcribing the room once instead of five times (#1388) needs the
five streams combined into one signal, and the only place all five exist in
phase-intact form is in RAM at ingest: the archive is Opus 32 kbps voip, which
does not preserve phase. So fusion must live in the process that receives the
raw PCM — this daemon.

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

1. **Ingest server** — port of `stream_server.py`, same wire protocol
   (`src/recall/wire.py` is the shared-facts reference), same archive layout,
   same pause behaviour, same `capture_events` evidence. **Deployed
   2026-09-04**: audiod is the live `recall-ingest` agent; the Python server
   stays in-tree as the rollback. The tier-1 aligner (`align.rs`,
   `envelope.rs`, the `align-probe` bin) is also **built and measured**: on
   the pre-epoch-fix June archive every 60 s block anchors at peak r
   0.66–0.94 with sub-second, smoothly drifting offsets.
2. **USB capture** — `audiod capture`: sox producer, metered pump,
   dead-segment watchdog, pause parking — the port of `recall record`
   (`runner.py` + `cli._cmd_record`). **Code built**; the deployment flip is
   deliberately last, because it is the sacred path (requirement #1) and TCC
   re-prompts on binary change. Replacing sox itself with an in-process
   CoreAudio read is a later, separate step behind the same watchdog.
3. **The fused `room` source** — per ~20 ms STFT frame, per band, weight each
   aligned source by local SNR; write ordinary Opus segments under `room/`.
   The worker then transcribes `room` first and per-source becomes backfill.
   The freed GPU budget (one transcription, not five) also makes non-turbo
   `large-v3` — the open better-on-Dutch lever in [pipeline.md §2](pipeline.md)
   — affordable on the room stream; the A/B harness to decide that already
   exists.
   Gated by a WER bake-off against best-single-mic on already-recorded audio
   (`recall.abcompare` + the human corrections corpus): the denoising lesson —
   afftdn and Demucs both measured *worse* than raw — says no "better" signal
   reaches the model without clearing that gate.
4. **Spatial features** — per-frame TDOA/level vectors across devices as
   sidecar files: a position fingerprint that separates same-voice/different-
   seat where voiceprints confuse, feeding diarization as a third view, and
   the mask source for person-filtered output (GSS-style).

The WER bake-off in (3) does not wait for (2): it runs offline over the
already-recorded archive (magnitude-tier alignment works on decoded Opus), so
the fusion question is answered before the sacred capture path is touched.
ffmpeg remains the segmenter child in (1) so rollback output stays directly
comparable with the Python server's; native Opus encoding arrives with (3),
which needs the PCM in process anyway.

## Deployment notes

- audiod must run as launchd `ProcessType=Interactive` like today's capture
  (design.md §7: `Background` is the throttled class and drops samples).
- Any new capture binary can re-trigger the macOS TCC mic prompt; expect
  `launchctl kickstart -k` after granting.
- The cutover per stage is: run in shadow, compare archives (the loss
  reconciler and the acoustic-loopback test are the referees), then flip the
  agent — and keep the Python path deleted only after its replacement has
  survived real days.
