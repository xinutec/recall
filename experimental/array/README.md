# array — the house's mics as one array

Run by hand, never deployed. The question: can the lossless captures from several
mics be combined, by physics rather than a model, into audio better than the best
single mic? A steered sum gains only where the mics share a coherent signal, so
this measures that first: each mic's alignment against a reference, and their
coherence per band once aligned.

```
cargo run --release -p array -- --root /Volumes/Backup/recall \
  --from 2026-10-22T08:21:40Z --to 2026-10-22T08:25:45Z \
  --reference usb --sources oneplus6t-0,oneplus6t-1,pixel5,iphone11 [--dump]
```

- Delays: GCC-PHAT on Hann-tapered frames within 300 Hz–4 kHz (a shared hum
  outside it outvotes the speech otherwise), sinc-peak interpolation. Coarse 4 s
  frames give each mic's clock line (offset, drift); 1 s frames are searched
  ±30 ms around it, since the talker's position moves the delay.
- Coherence: Welch, 1024-point Hann, over the louder half of the frames, each
  steered by its own delay.
- `tests/align.rs` proves each step on signals with a known answer; the run
  reports the reference against itself as a positive control.

## First result, 2026-10-22 08:21–08:25 (a phone call, six mics, all FLAC 48 kHz)

- pixel9 recorded digital silence; the other five carried the call.
- The mics hear the same speech: a spectrogram shows the same syllables at the
  same moments, and the speech-band envelopes of usb and oneplus6t-0 correlate
  0.90 at −1.21 s, the offset the tool found.
- Their waveforms do not correlate at any delay within ±30 ms of that offset:
  0.004 over the minute, at most 0.04 in 2 s windows; coherence ≈ 0 in every
  band, against 1.00 for the control.

Not yet told apart:
1. Room physics: a few metres from the talker each mic hears mostly
   reverberation, which is uncorrelated between points metres apart. Then a
   steered sum of these mics gains nothing, and only a close mic or a compact
   array (one clock, centimetres apart) can.
2. The capture path: a phone that fell back from `UNPROCESSED` to processed
   `MIC`, or a stream that drops samples, keeps the envelope and scrambles the
   waveform. The app does not log which source it opened.

The test that separates them: two phones side by side (~10 cm) and a mic 2–3 m
away, someone speaking at 1 m. Side by side must cohere strongly below ~1 kHz if
the capture path is sound; how much the distant pair keeps is the room's answer.
