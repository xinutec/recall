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

   `device` is the `CoreAudio` output's name. `{"mix": [<voices>, ...]}`
   alternates turns between sources, e.g. English readers and FLEURS Dutch,
   for speech that switches language turn by turn. Turns go round the voices in
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
   `DIR/played.jsonl`. The Mac ramps its output up over about half a second
   when playback starts, so each part opens with `--lead` (0.5 s) of silence
   before its first turn's own gap. Whether the ramp counts silence is
   untested: the score shows whether each part's first words were heard.

   ```sh
   playback play --dir DIR
   ```

5. Wait until every minute of the window has lines in `recall.sqlite`, not
   only until its jobs are done: the turn writer adds them in batches, up to
   an hour after a minute's job finished. Then copy the window's
   `audio_segments` and their `transcript_segments` into a small file (sqlite
   `ATTACH` the fleet's file read-only, `CREATE TABLE ... AS SELECT`) and score:

   ```sh
   playback score --dir DIR --db window.sqlite [--json]
   ```

   Per source: word error rate over all parts, its substitutions, deletions
   and insertions, the rate per part, and words "invented": in lines that sit
   in no part, said while nothing was played. Only shown lines count: the line
   diarization rewrote is hidden and its rewrite shown, so both would count the
   same speech twice. A phone's clock can sit a few seconds off, so a line at a
   part's very edge can land in the silence and read as invented: check those.

6. Score the fleet's diarization of the window against who was played
   (`LibriSpeech` turns only; FLEURS names no speakers). Export the window's
   `diarize-segment` jobs from `ingest.sqlite` as a JSON array of
   `{filename, source, result}` (`sqlite3 -json`), then:

   ```sh
   playback diarization --dir DIR --results diarize.json [--vad]
   ```

   Per source: speech no turn covers, time a label sits on a reader other than
   its own, turns spanning two readers, and minutes whose label count differs
   from the readers heard. `--vad` trims each played turn to where its
   recording has speech, so leading silence and pauses do not count as missed.

Numbers are spelled as words in `LibriSpeech`'s reference and usually as
digits by Whisper, which counts against every arm alike: compare arms, do not
read the absolute rate as the transcriber's.

The played window's lines are machine transcripts of test speech in the
household's record: once scored, hide the window's shown machine lines with
`hidden_reason = 'test speech played into the house'` (`HiddenReason::PlayedTest`),
after a snapshot. The audio stays.
