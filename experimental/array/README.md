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

## Second look, same call: it is the room, and the shared signal is in the echo

Aligned every 2 s and measured with 171 ms windows instead of 21 ms, usb and
each phone share about 30% coherent power across 300 Hz–4 kHz (21 ms: 0.02–0.09;
the estimate's floor is about 0.05):

| usb against | 100–300 | 300–1k | 1–2k | 2–4k | 4–8k |
|---|---|---|---|---|---|
| oneplus6t-0 | 0.25 | 0.34 | 0.35 | 0.31 | 0.15 |
| oneplus6t-1 | 0.21 | 0.33 | 0.28 | 0.28 | 0.12 |
| pixel5      | 0.20 | 0.32 | 0.32 | 0.29 | 0.09 |

- The capture path keeps the waveform: a scrambled one could not gain coherence
  with a longer window.
- One talker's echo is that talker through each mic's room response, a linear
  filter hundreds of milliseconds long. A short window cannot see it; a long one
  can. At these distances most of the shared signal is echo, not direct sound,
  which is why delay-and-sum (the direct path only) found almost nothing.
- So the combiner to try is a multichannel Wiener filter on long frames: per
  frequency, each mic's relation to the reference's speech is estimated from the
  recordings and the mics are combined through it. Its gain is bounded by this
  coherence.

## The loudspeaker test, 2026-10-23 21:47 (failed)

White noise and read speech from the living room's left and right outputs
(`Khonsu L`/`R`), at the volume as set: most mics did not rise above their floor
(usb not at all). Where one did (oneplus6t-0, about 9 dB), the played noise was
not found in it by correlation at any lag (peak 0.003), so the speaker path does
not keep the waveform, as a lossy link would not. Loudspeakers are no source for
a waveform comparison here; speech in the room is.
