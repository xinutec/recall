# Speech fixtures

Two kinds live here, and the difference is the whole point.

## `public-domain-en.flac` — COMMITTED, runs everywhere

48 s of read poetry, 16 kHz mono FLAC. This is the fixture CI, the nix sandbox
and any fresh clone can actually use.

- **Recording:** LibriVox, *Short Poetry Collection 001*, "Because I Could Not
  Stop For Death", read by the volunteer `wedschild`. LibriVox releases its
  recordings into the **public domain**; the clip says so in its own first ten
  seconds, so the provenance travels with the artefact.
  Source: `archive.org/details/short_poetry_001_librivox`.
- **Text:** Emily Dickinson, died **1886**. Public domain **worldwide**, not
  merely in the USA — which is why this poem and not the Robert Frost track in
  the same collection: Frost died in 1963, so "The Road Not Taken" is still
  restricted in the EU until 2034, and `recall` is a public repository read from
  the Netherlands.
- Converted with `ffmpeg -ac 1 -ar 16000 -c:a flac`, matching what every
  recorder delivers and what the room builder emits.

⚠ `public-domain-en.txt` is the CANONICAL poem text plus the spoken LibriVox
preamble — it is not a hand-verified transcription of this particular reading.
A reader's small deviations therefore show up as a little WER. That is fine for
a DRIFT check, which asks whether the number MOVED, not whether it is zero; it
would not be fine for an absolute quality claim, so do not make one from it.

## `dialogue-*.flac` — machine-read, and committed since 2026-09-09

⚠ **These are NOT recordings of anyone.** They are macOS `say` reading INVENTED
lines — plants, a plumber, a bakery — rendered by `scripts/gen-speech-fixture.sh`
and fully regenerable from it. Two English voices and one Dutch, stitched with
0.8 s gaps into the shape of a captured segment (48 kHz mono).

An earlier version of this file called them "recordings of real people in this
household". That was wrong, and it mattered: it made the absence look deliberate
and correct, when in fact `.gitignore`'s blanket `*.flac` swallowed fixtures the
generator's own header calls committed. The ignore was narrowed on 2026-09-09 and
they now travel with the repo, so the Dutch ASR check and the Dutch VAD test run
on any clone. The reference transcripts beside them ARE
committed, which is what made the gap easy to miss — `score-asr` advertised a
"committed speech fixture" for months while the audio existed on one Mac (#1433).

They are the better ASR drift instrument of the two kinds here, because their
references are EXACT rather than approximate: measured 2026-09-06 with
large-v3-turbo, en 0.0123 and nl 0.0000, against 0.0426 for the reading above.
They also carry the only Dutch in the gate.

Tests that need them skip when they are absent, and say so.
