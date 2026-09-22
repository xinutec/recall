# recall

Local, always-on household speech recall: records the house, transcribes it,
attributes who said what, and makes it searchable. A memory aid. Everything
stays on the household's own machines.

- [`docs/architecture.md`](docs/architecture.md): what it is for and how it is built
- [`docs/running.md`](docs/running.md): the agents, the fleet, deploying
- [`docs/devices.md`](docs/devices.md): phone and microphone ingest, identity, liveness
- [`docs/meetings.md`](docs/meetings.md) and [`docs/meeting-recorder.md`](docs/meeting-recorder.md): one-off recordings
- [`docs/review.md`](docs/review.md): reading a session from the terminal
- [`docs/conventions.md`](docs/conventions.md): how the code is written and checked

## Layout

A Rust workspace: `audiocore` (what the crates share), `audiod` (the Mac's audio
plane), `recalld` (the fleet's system of record and the web API), `runner` (the
Mac's job loop and the live feed), `doctor` (the Mac's health agent), `cli`.
`src/recall` is the Python model floor: the two shims the runners drive, the
golden ASR check, and `llm-host`. `frontend/` is the Angular app; `android/` and
`ios/` the microphone apps.

## Dev

```sh
nix develop                               # rust, python, node, ffmpeg, sox, the lot
nix run ../dev-lint#gate -- . gate.json   # the full gate; a commit runs it
```

`.venv` is a symlink into the nix store, built by `nix build .#dev-env
--out-link .venv` from `uv.lock`. Run a Python module with
`nix develop --command env PYTHONPATH=src .venv/bin/python -m recall.<module>`;
without `PYTHONPATH=src` you run the store's copy, not your edit.
