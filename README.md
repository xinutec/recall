# recall

Always-on household speech recall: records the house, transcribes it,
attributes who said what, and makes it searchable.

- [`docs/architecture.md`](docs/architecture.md): what it is for and how it is built
- [`docs/running.md`](docs/running.md): the agents, the fleet, deploying
- [`docs/devices.md`](docs/devices.md): phone and microphone ingest, identity, liveness
- [`docs/meetings.md`](docs/meetings.md) and [`docs/meeting-recorder.md`](docs/meeting-recorder.md): one-off recordings
- [`docs/review.md`](docs/review.md): reading a session from the terminal
- [`docs/conventions.md`](docs/conventions.md): how the code is written and checked

## Layout

A Rust workspace: `audiocore` (shared), `audiod` (the Mac's audio plane),
`recalld` (the fleet's system of record and web API), `runner` (the Mac's job
loop and live feed), `doctor` (the Mac's health agent), `cli`, `transcript`
(the domain as pure data), `experimental/playback`. `src/recall` is
the Python the runners drive (the model shims) and the ASR check.
`frontend/` is the web app; `android/` and `ios/` the microphone apps.

## Dev

```sh
nix develop                               # rust, python, node, ffmpeg, sox
nix run ../dev-lint#gate -- . gate.json   # the full gate; a commit runs it
```

`.venv` is a store symlink (`nix build .#dev-env --out-link .venv`, from
`uv.lock`). Run a Python module with
`nix develop --command env PYTHONPATH=src .venv/bin/python -m recall.<module>`;
without `PYTHONPATH=src` it runs the store's copy, not your edit.
