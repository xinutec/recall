# Speech fixtures

All committed; tests that need one skip, saying so, when it is absent.

## `public-domain-en.flac`

48 s, 16 kHz mono. LibriVox *Short Poetry Collection 001*, Emily Dickinson's
"Because I Could Not Stop For Death" (archive.org
`short_poetry_001_librivox`): public-domain reading of a text public domain
worldwide, unlike the Frost track beside it, restricted in the EU until 2034.

`public-domain-en.txt` is the published poem plus the LibriVox preamble, not a
transcription of this reading, so it scores some WER (0.0426): good for drift,
not for absolute quality.

## `dialogue-en.flac`, `dialogue-nl.flac`

macOS `say` reading invented lines, from `scripts/gen-speech-fixture.sh`; 48 kHz
mono, like a captured segment. `reference-*.txt` are exact, so they are the
better drift check (en 0.0123, nl 0.0000 with large-v3-turbo, 2026-09-06), and
the only Dutch in the gate.
