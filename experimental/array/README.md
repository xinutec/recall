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

## Measured right: the mics share most of the waveform

A first pass reported coherence ≈ 0, then 0.3: both were my alignment errors (a
coarse search that could not reach a later mic, and a fine alignment off by its
own search margin, 10–50 ms). Each run below has the reference against itself
through the same code as a control, at 1.00 in every band.

Coherence with 171 ms windows, each 2 s steered by its own measured delay
(estimate floor about 0.05):

**The 22 Oct call** (speech, two talkers), against usb:

| mic | 100–300 | 300–1k | 1–2k | 2–4k | 4–8k |
|---|---|---|---|---|---|
| oneplus6t-0 | 0.40 | 0.60 | 0.57 | 0.52 | 0.22 |
| oneplus6t-1 | 0.30 | 0.58 | 0.58 | 0.51 | 0.21 |
| pixel5      | 0.26 | 0.47 | 0.51 | 0.43 | 0.12 |
| iphone11    | 0.05 | 0.04 | 0.04 | 0.05 | 0.04 |

iphone11 aligned with nothing on the call; on the loudspeaker test it is the
best mic. Not yet explained.

**White noise from the right loudspeaker, 2026-10-23 21:59** (one fixed
source), against oneplus6t-0:

| mic | 100–300 | 300–1k | 1–2k | 2–4k | 4–8k |
|---|---|---|---|---|---|
| iphone11    | 0.96 | 0.82 | 0.84 | 0.72 | 0.73 |
| oneplus6t-1 | 0.25 | 0.62 | 0.69 | 0.63 | 0.76 |
| usb         | 0.37 | 0.68 | 0.64 | 0.56 | 0.62 |
| pixel5      | 0.21 | 0.38 | 0.51 | 0.49 | 0.55 |
| geb         | 0.32 | 0.42 | 0.10 | 0.06 | 0.05 |

- Most pairs sit near the ceiling their own noise floors allow: the room costs
  little at 171 ms. A room's echo is a linear filter of the source, so a window
  long enough to hold it keeps the relation; at 21 ms most of it is lost.
- geb loses coherence above 1 kHz and its delay wanders by up to 14 ms within
  16 s; its capture path needs a look before it joins a combination.
- The loudspeaker path itself loses the waveform (the played file is not found
  in any mic), so loudspeaker tests compare mics with each other, never with the
  file. At the volume first set, most mics did not hear them at all.

So the combiner to build is a multichannel Wiener filter on long frames: per
frequency, each mic's relation to the reference's speech, estimated from the
recordings, and the mics combined through it.
