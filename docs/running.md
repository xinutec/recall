# Running recall

Everything operational is a Rust binary: `audiod` (capture, ingest, upload),
`recalld` (the fleet daemon), `doctor`, `runner` and `recall-live`,
`recall-cli`. The Python model floor runs from `.venv`, the interpreter the
agents run (see the [README](../README.md)). The Mac's data root is
`/Volumes/Backup/recall` (encrypted).

## The Mac's agents

`deploy/hm-agents.nix` defines them; home-manager installs them. Apply a change
with: commit here, then in `~/.config/home-manager` run
`nix flake update recall && home-manager switch --flake .#$USER`. An agent
runs the pinned store revision, so editing the tree changes nothing until that
switch. Restart one with `launchctl kickstart -k gui/$(id -u)/<label>`. Logs are
in `~/Library/Logs/recall/<agent>.{out,err}.log`; `recall-logrotate` caps them.

| agent | does |
|---|---|
| `recall-capture` | USB mic → gap-free FLAC segments; always on |
| `recall-ingest` | one TCP server (port 9999) for every phone mic |
| `recall-upload` | closed segments → recalld, receipts re-hashed before anything counts as delivered |
| `recall-capture-mirror` | reports what capture applied and long-polls Isis for the pause intent, mirrored onto the local pause file |
| `recall-live` | the tap → speech detection → transcribe what was just said → `POST /sync/live` |
| `recall-runner` | leases `transcribe-*` jobs, drives the `asr` shim, pushes the result |
| `recall-voices` | leases `diarize-segment` and `enroll-speaker`, drives the `voices` shim |
| `recall-doctor` | the health checks, every five minutes, reported to fleetwatch |
| `recall-beat-relay` | accepts a mic app's heartbeat on the LAN (port 8000) and forwards it to Isis |

The agents read `~/.config/recall/env` (0600, on the internal disk), not the
repository's `.env`: a launchd agent cannot touch `/Volumes/Backup`, and its
first touch can hang the agent rather than fail it. Keep the two files in step
by hand. `recall-upload` needs `RECALL_INGEST_TOKEN`; the mirror, the runners
and the doctor need `RECALL_SYNC_TOKEN`; the doctor also needs the fleetwatch
token.

### The doctor runs itself twice

`doctor` starts a child (`--collect`) that does every read of the archive
volume, the pause file included, and prints its checks as JSON; the parent
touches only launchd, `~/.config` and Isis. A child silent for 60 s is abandoned
and reported as `archive answers: no answer`, with its pid and state on stderr;
in `U` state it cannot be killed and exits when the volume answers. The stalls
come from other writers on the volume (#1412).

### The runner leaves a pulse

`<data root>/worker-heartbeat.json` is stamped after every job and the doctor
grades its age (warn 30 min, fail 1 h). The name predates the runner; the
fleet's history keys on it. An empty queue stamps too.

Only capture needs the microphone grant. A denied grant is digital silence, not
an error: segments are written and every one is silent. Look at levels, not
logs: `doctor --out <root> --collect` prints them.

## The fleet

recalld runs in one container on Isis, on one port behind isis's front door:
`https://recall.xinutec.org`, on the VPN only, serves the app (behind the
Nextcloud sign-in), ingest, sync and the job queue ([k8s](../deploy/k8s/README.md)).
Shipping a change: push to `main`, CI builds `xinutec/recall:latest`, then
`ssh root@10.100.0.2 'kubectl -n recall rollout restart deployment/recall'`.

```sh
./scripts/recall-build-frontend.sh                          # the image's frontend build, locally
nix develop --command bash -c 'cd frontend && pnpm start'   # dev server, proxies /api
```

## Capture

The USB mic is pinned by CoreAudio device name in `deploy/hm-agents.nix`. Never
record from the default input: macOS re-points it at whatever connects. A
renamed or missing device makes sox fail and the agent crash-loop, visibly.

A pause stops all recording, USB mic and phones: the ingest server closes its
listener and drops active streams, finalising the current segment. The web
app's pause is mirrored to the Mac within seconds; `audiod pause --root <root>`
and `audiod resume` are the break-glass when Isis cannot be reached.

Phones: [android](../android/README.md), [ios](../ios/README.md); protocol and
liveness: [devices.md](devices.md).

## Speakers and vocabulary

Naming a voice in the app files a correction that enrols it; recalld derives an
`enroll-speaker` job and the voiceprint is built on the Mac. There is no
separate enrol screen. Names, places and terms the transcriber should spell
right are managed on the Labels page and applied as Whisper's `initial_prompt`
from the next job. Diarization and embeddings are gated models: accept the
terms on `pyannote/speaker-diarization-3.1`, `pyannote/segmentation-3.0` and
`pyannote/embedding`, and put a read token in the env file as `HF_TOKEN`.

## The golden ASR check

```sh
nix develop --command env PYTHONPATH=src .venv/bin/python -m recall.score_asr
```

Transcribes the three committed fixtures with the real model and fails if word
error rate drifts past each one's threshold or the language is mis-detected.
On demand, not in the gate: it loads the model.

## The shim by hand

```sh
echo '{"id":"1","op":"transcribe","audio":"tests/fixtures/speech/public-domain-en.flac"}' \
  | PYTHONPATH=src .venv/bin/python -m recall.shim_asr
```

Its stdout is the protocol; model chatter goes to stderr.
