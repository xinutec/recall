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

## `dialogue-*.flac` — NOT committed, local only

Recordings of real people in this household. `.gitignore` refuses `*.flac` under
"NEVER commit audio or transcripts", and that rule is right: this repository is
public. The reference transcripts beside them ARE committed, which is what made
their absence easy to miss — `score-asr` advertised a "committed speech fixture"
for months while the audio existed on exactly one Mac (#1433).

Tests that need real household speech skip when these are absent, and say so.
