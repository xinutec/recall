# recall

Local, always-on household speech recall system — a memory aid that records,
transcribes, attributes (who said it), and makes searchable the conversations
in the house. Everything stays on-device.

- **Running it (services, web app, HF setup, enrollment, daily flow):** [`docs/running.md`](docs/running.md)
- **Design:** [`docs/design.md`](docs/design.md)
- **Analysis pipeline (ASR, EN/NL, speakers, reprocessing):** [`docs/pipeline.md`](docs/pipeline.md)
- **Phone/mic ingest (connect, identity, liveness):** [`docs/devices.md`](docs/devices.md)
- **Reading a recorded call from the CLI:** [`docs/review.md`](docs/review.md)
- **Ingesting discrete meeting recordings (download → ingest → transcribe → clean):** [`docs/meetings.md`](docs/meetings.md)
- **Conventions (strict typing, TDD):** [`docs/conventions.md`](docs/conventions.md)

## Web app

Open **`http://10.100.0.2:8000`** (Isis, over the VPN, behind a Nextcloud sign-in):
a timeline of the conversation, full-text search with audio playback, a
review/correct queue, record-from-device (phone as a second mic), and speaker
labelling that enrols voices as you confirm who spoke. It's an Angular app served
by FastAPI on one origin, running on Isis — the Mac serves no UI.
See [`docs/running.md`](docs/running.md).

## Dev

Backend (Python) in the Nix devshell:

```sh
nix develop          # python + mypy + ruff + pytest + ffmpeg + sox + uv + node
./scripts/verify.sh   # the full gate: ruff/format/mypy/dev-lint/contract/pytest/frontend
```

Frontend (Angular 22, in `frontend/`):

```sh
./scripts/recall-build-frontend.sh           # build into dist/ (served by the API)
nix develop --command bash -c 'cd frontend && npx ng serve'   # dev, proxies /api -> :8000
nix develop --command bash -c 'cd frontend && npx ng test --watch=false'
```

ML deps (mlx-whisper, pyannote) live in a uv-managed `.venv`; `scripts/recall.sh`
runs any command with the full environment.

## Status

Live and self-running, split across two machines: the Mac mini does capture and
live + batch transcription as launchd services, recording the household to an
encrypted disk, and pushes to Isis, which is the system of record and serves the
web app.
Per-turn EN/NL transcription works; speaker attribution turns on once a
HuggingFace token is set and voices are enrolled (see `docs/running.md`).
Transcripts are versioned and re-derived, never overwritten, so accuracy improves
over time without losing history.
