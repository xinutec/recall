# array — the house's mics as one array

Run by hand, never deployed. The question: can the lossless captures from several
mics be combined, by physics rather than a model, into audio better than the best
single mic? A steered sum gains only where the mics share a coherent signal, so
this measures that first: each mic's alignment against a reference, and their
coherence per band once aligned.

```
nix develop -c cargo run --release -p array -- measure --root /Volumes/Backup/recall \
  --from 2026-10-22T08:21:45Z --to 2026-10-22T08:25:40Z \
  --reference usb --sources oneplus6t-0,oneplus6t-1,pixel5,iphone11
nix develop -c cargo run --release -p array -- combine … --out DIR   # WAVs + levels
```

`measure`: each mic's clock against the reference (`clock::locate`: a loud 20 s
located coarsely, then every loud 2 s chunk refined, and a line through them;
offset only under a minute) and its coherence per band over 171 ms windows.
`combine`: the mics resampled onto the reference's clock (`resample::onto`), a
171 ms STFT, and a multichannel Wiener filter (`mwf`) estimating the speech as
the reference hears it; each input and the result written as WAV with its
speech-to-background ratio.

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

(iphone11 on the call is 6.7 s off the others, outside the search I first
used; `array measure` finds it, at 0.37/0.58/0.59/0.50/0.20.)

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

## The Wiener filter, 2026-10-24: real but small, because noise is not the problem

Speech-to-background ratio in the speech band, speech frames (reference's
loudest 40%) against background frames (its quietest 30%):

| stretch | best single mic | combined, that mic as reference |
|---|---|---|
| loudspeaker speech, 23 Oct 21:59:40–22:00:20, 5 mics | usb 32.1 dB | 33.5 dB (+1.4) |
| the call, 22 Oct 08:21:45–08:25:40, 5 mics | usb 23.5 dB | 23.6 dB (+0.1) |

The background is already 23–32 dB below the speech, so there is little noise
to take away. What a far mic loses is the room: its reverberation. This filter
estimates the speech as the reference mic hears it, reverberation included, by
design, so it cannot touch that.

The physics that can: with two or more mics a room can be inverted exactly by
FIR filters (the multiple-input/output inverse theorem, MINT), which no single
mic allows, since a room's response is not minimum-phase. The classical method
that uses it is multichannel linear prediction (weighted prediction error,
WPE): it predicts each mic's late reverberation from all the mics' past
samples and subtracts it. That is the next thing to try, scored by a
reverberation measure, by ear, and by word error rate on hand-checked minutes.
