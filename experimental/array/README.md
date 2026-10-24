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
nix develop -c cargo run --release -p array -- dereverb … --out DIR  # WPE at 16 kHz
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
- geb's figures above came from that first, flawed pass. On the call `array
  measure` aligns geb like the phones (drift −17 ppm; coherence with usb
  0.38/0.56/0.49/0.35/0.16). On the loudspeaker test it barely heard the
  speakers over its own floor, which sits 20–30 dB above the phones' (−45 to
  −57 dBFS, most likely the machine's fans beside its mic). No fault; the
  noisiest mic.
- A drift figure under about 20 ppm is noise: over 4 minutes a talker's moves
  shift the delays by a millisecond (the same phone fitted +12 and −2 ppm in
  two runs).
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

## Dereverberation (WPE), 2026-10-24

`array dereverb`: the aligned mics at 16 kHz (`decimate`), 32 ms frames every
8 ms, and multichannel linear prediction (`wpe`): each mic's echo from 24 ms on
is predicted from every mic's past frames and subtracted. On a synthetic room
with a 300 ms tail (`tests/dereverb.rs`) one mic cuts the late echo by 0.7 dB
and four mics by 19.9 dB: what one point cannot tell from the speech, several
can.

## Speech of known text, 2026-10-24 10:37–10:43: nothing left to win

LibriSpeech readers through the left speaker, then the right (2½ min each, the
volume as set), transcribed with recall's own path (`recall.asr.mlx_transcribe`,
whisper-large-v3-turbo) and scored against the corpus text:

| | left (358 words) | right (355 words) |
|---|---|---|
| usb | 1.7% | 2.8% |
| iphone11 | 1.7% | 1.1% |
| oneplus6t-0 / -1 | 2.2% / 1.7% | 1.4% / 2.0% |
| pixel5 | 23.5% | 31.0% |
| Wiener, usb reference | 2.0% | 1.1% |
| WPE, usb alone | 1.7% | 1.7% |
| WPE, all five | 1.7% | 2.5% |

Every mic but pixel5 already transcribes this at 1–3%: 4 to 10 errors, the
differences a handful of words. Speech this clear and this loud is not limited
by the room, so neither combiner can show a gain on it. The test that can is
harder material: the same corpus played much quieter (a distant or soft
talker), or hand-checked conversation (#1470).

## Quiet speech and a real voice, 2026-10-24: a gain only at the edge of audibility

The same kind of corpus speech from the left speaker, one minute, played 15 and
25 dB below the run above (the file quieter, the volume as set); then a person
in the room reading the Rainbow Passage twice, normal and quiet/far. Word
errors:

| | corpus −15 dB (138 w) | corpus −25 dB | voice, normal (98 w) | voice, quiet/far |
|---|---|---|---|---|
| usb (best single) | 25.4% | 75.4% | 0% | 4.1% |
| iphone11 | 25.4% | 99% | 2.0% | 3.1% |
| oneplus6t-0 | 26.1% | 99% | 2.0% | 3.1% |
| Wiener, usb reference | 27.5% | **60.1%** | 0% | 4.1% |
| WPE, usb alone | 25.4% | 67.4% | 0% | 7.1% |
| WPE, all five | 24.6% | 70.3% | 2.0% | 4.1% |

(The −15 dB errors are real: hard, poetic sentences at a low level.)

- A voice in the room, quiet or from across it, is already transcribed at 0–6%
  by almost any single mic: there is nothing left to win.
- Combining helps only near the edge of audibility: at −25 dB, 83 errors
  against the best mic's 104, though three phones alone heard nothing.
- So at home the room's acoustics are not what limits accuracy, and recall's
  best-mic-per-stretch already captures nearly all of it. Parked here: the tool
  is ready if real conversation shows acoustic errors it could fix (#1470).
