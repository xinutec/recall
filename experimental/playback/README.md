# playback: known speech, scored per microphone

An experiment (#1388): play speech from a public test set through the house's
loudspeakers while capture runs, then score each microphone's transcript
against the text that was played. The household's own speech has no
reference unless someone checks every word; played speech brings its own, so
microphones, and ways of combining them, can be compared with nobody at home.

Never deployed. It does not touch capture: resuming and pausing it is the
household's decision, made before `play` and after it.

## Run it

1. Fetch the corpora outside the repo:
   - `LibriSpeech` test-clean (English read speech, CC BY 4.0),
     `https://www.openslr.org/resources/12/test-clean.tar.gz`; pass the
     `test-clean` directory.
   - FLEURS (CC BY 4.0), per language from
     `https://huggingface.co/datasets/google/fleurs/resolve/main/data/<lang>/`:
     `test.tsv` and `audio/test.tar.gz`, laid out as `<dir>/<lang>/test.tsv` and
     `<dir>/<lang>/test/*.wav`; pass `<dir>`.

2. Describe the parts, each a stretch of turns from one output device:

   ```json
   [
     {"name": "en-L", "device": "Khonsu L", "seconds": 300,
      "voices": {"librispeech": ["1089", "121", "2300", "1580"]}},
     {"name": "nl-L", "device": "Khonsu L", "seconds": 240,
      "voices": {"fleurs": "nl_nl"}}
   ]
   ```

   `device` is the `CoreAudio` output's name. Turns go round the voices in
   order, 4 to 15 s each, 0.4 to 1.5 s apart, peaks at -3 dBFS; the device's
   own volume is left as it is.

3. Build: writes `DIR/plan.json` (every turn's offset and text) and one WAV
   per part. A seed rebuilds the same plan.

   ```sh
   playback build --spec parts.json --dir DIR --librispeech .../test-clean \
       --fleurs .../fleurs --seed 20261001
   ```

4. With capture running, play: silence, then each part followed by silence
   (`--gap`, 45 s), through `sox`. Each part's start goes to
   `DIR/played.jsonl`.

   ```sh
   playback play --dir DIR
   ```

5. Once the window is transcribed, copy the fleet's `recall.sqlite` with
   sqlite's `.backup` (read-only, never the live file) and score:

   ```sh
   playback score --dir DIR --db recall.sqlite [--shown] [--json]
   ```

   Per source: word error rate over all parts, its substitutions, deletions
   and insertions, the rate per part, and words "invented": in lines that sit
   in no part, said while nothing was played. `--shown` scores only the lines
   the household is shown.

Numbers are spelled as words in `LibriSpeech`'s reference and usually as
digits by Whisper, which counts against every arm alike: compare arms, do not
read the absolute rate as the transcriber's.

The played window's lines are machine transcripts of test speech in the
household's record: hide them once scored.
