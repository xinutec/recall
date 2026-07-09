# Running recall

All ML commands run via `scripts/recall.sh <cmd>` (Nix tools + the `.venv` with
mlx-whisper/pyannote + `HF_TOKEN` from `.env`). Data root is
`/Volumes/Backup/recall` (encrypted), passed as `--out`.

## Services (launchd)

The agents are defined in `deploy/hm-agents.nix` and installed by **home-manager**
— there are no hand-written plists. Apply a change with: edit that module, commit,
then in `~/.config/home-manager` run
`nix flake update recall && home-manager switch --flake .#pippijn`. Restart one
ad-hoc with `launchctl kickstart -k gui/$(id -u)/<name>`. Logs:
`logs/<agent>.{out,err}.log`; health: `./scripts/recall.sh doctor` (checks every
agent is loaded).

| agent | does | when |
|---|---|---|
| `com.pippijn.recall-capture` | USB mic → gap-free Opus segments | always on |
| `com.pippijn.recall-live` | VAD → transcribe each utterance (~2–3 s, provisional) | always on |
| `com.pippijn.recall-worker` | index + transcribe new segments (whole-clip; diarization is the refine agent's job) | continuous |
| `com.pippijn.recall-api` | Angular web app + JSON API on `:8000` | always on |
| `com.pippijn.recall-tunnel` | reverse SSH tunnel publishing the web app on Isis's WG IP (`10.100.0.2:8000`) for off-LAN VPN access | always on |
| `com.pippijn.recall-ingest` | one TCP server (port 9999) for all phone mics | when phones used |
| `com.pippijn.recall-refine` | re-derive segments diarized + speaker-split; also drains queued A/B model comparisons | diarize: while capture paused · A/B: any time |
| `com.pippijn.recall-backup` | mirror the archive to odin (snapshot DB + rsync audio) | nightly 23:30 |

The archive's only unrecoverable copy is this volume, so the backup agent mirrors
it nightly to `odin:/backup/recall-mirror` (`scripts/recall-backup.sh`: consistent
SQLite snapshot + rsync of the audio, no `--delete` so deletions never propagate).
`recall doctor` fails when the last successful mirror is older than 48h.

> **macOS mic permission is per-agent:** capture and live each need their own
> grant. If an err log shows `Out:0`, allow the prompt (or System Settings →
> Privacy → Microphone) and `kickstart -k`.

## Web app (`:8000`)

`http://<mac-ip>:8000` (LAN) — timeline, full-text search with playback,
review/correct queue, phone-as-mic recording, and speaker labelling (which enrols
voices as you confirm who spoke).

**Off-LAN access.** The same app is reachable at `http://10.100.0.2:8000` from any
device on the WireGuard VPN. The `recall-tunnel` agent (`scripts/recall-tunnel.sh`)
holds a reverse SSH tunnel that publishes the Mac's `:8000` on Isis's WG address:
the Mac is a one-way VPN peer nothing can dial into, so it dials out and forwards
the app backwards. It rides WireGuard end to end — WG peer keys are the auth and WG
encrypts the wire, so there is no TLS and a client only needs its own WG tunnel up.
Plain http means no installable service-worker/PWA mode; everything else works.
Isis serves it on its WG IP only (the public interface stays closed), enabled by
`GatewayPorts clientspecified` on its sshd (in `nixos-config`); the Mac's own WG
tunnel is kept connected by the `wg-ensure` agent (in `xinutec-infra`).

```sh
./scripts/recall-build-frontend.sh    # rebuild UI; the service serves it live, no restart
nix develop --command bash -c 'cd frontend && npx ng serve'   # dev: hot reload, proxies /api
```

## Capture & verify

USB mic → `/Volumes/Backup/recall/`, gap-free, auto-restarts.

Capture and live both pin the mic with `--device "USB Condenser Microphone"`
(`scripts/recall-capture.sh`, `deploy/hm-agents.nix`). Never record from the
*default* input: macOS re-points it at whatever connects, e.g. a Bluetooth
speaker's hands-free mic — which then chimes into call mode and records at
telephone quality. A renamed/missing device makes sox fail hard and the agent
crash-loop (visible in `logs/capture.err.log`) rather than silently recording
from the wrong mic.

```sh
launchctl bootout   gui/$(id -u)/com.pippijn.recall-capture                                # stop
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.pippijn.recall-capture.plist   # start
nix develop --command python -m recall verify --out /Volumes/Backup/recall                # check for gaps
```

A **pause** stops *all* recording (USB mic and phones): the ingest server closes
its listener and drops active streams, finalising the current segment cleanly —
nothing is recorded against a pause.

## Phone mics

Spare Android phones running the `recall-mic` app stream PCM over TCP to the one
ingest server (port 9999), segmented like the USB mic. **Adding a phone is
phone-side only** — install the app, set the host, press Start; it self-registers
on first connect. Protocol: [devices.md](devices.md). Build/install:
[`android/README.md`](../android/README.md).

## Worker, live & refine

The worker runs continuously, picking up new audio within seconds: index new
segments and transcribe the untranscribed whole-clip — diarization is left to the
refine agent so it never competes with live capture; done work is never redone.
Live transcripts are provisional and superseded by the worker, then by refine,
then by human corrections — search always shows the current best.

**Refine** re-derives segments the diarized way (split into speaker turns) and
supersedes the merged ones — human corrections kept, nothing deleted. Heavy
(~2.5 CPU-min/audio-min, ~6 GB), so it runs **only while capture is paused** and
yields the moment it resumes; newest-first, resumable. Needs `HF_TOKEN`.

```sh
./scripts/recall.sh worker  --out /Volumes/Backup/recall                  # run a pass now
./scripts/recall.sh refine  --out /Volumes/Backup/recall --max-segments 5
./scripts/recall.sh search "coffee" --out /Volumes/Backup/recall
```

## One-time: HuggingFace (diarization + embeddings are gated)

1. Free account at <https://huggingface.co>; accept the terms on each model page:
   `pyannote/speaker-diarization-3.1`, `pyannote/segmentation-3.0`,
   `pyannote/embedding`.
2. Create a read token, put it in `.env` (gitignored): `echo 'HF_TOKEN=hf_xxx' > .env`.

## Enrol the household

Enrolment is additive and mostly automatic: tagging who said a turn (in **Train**, or
on a session) files a correction that enrols that voice, so the roster builds as you
label — there's no separate enrol screen. To seed a voice up front from a clean clip,
use the CLI:

```sh
./scripts/recall.sh enroll --name Alex --audio alex.wav --out /Volumes/Backup/recall
```

## Daily flow

Capture + transcription run themselves. What's left is occasional: correct
transcripts in the **Review** screen (accumulates training data), then:

```sh
./scripts/recall.sh identify   --out /Volumes/Backup/recall   # attribute turns to enrolled people
./scripts/recall.sh transcript --out /Volumes/Backup/recall   # list / read sessions — see review.md
```

## Recall layer (summaries + Ask)

The refine daemon writes one summary per finished day (`day_summaries`; local
LLM via mlx-lm, model in `recall.llm.DEFAULT_LLM`). The web app's **Ask** page
answers questions grounded in FTS-retrieved turns, cited; backfill or redo a day
by hand:

```sh
./scripts/recall.sh summarize --out /Volumes/Backup/recall [--day 2026-06-28]
```

## Vocabulary (proper nouns)

Names, places and terms the transcriber should spell right are managed on the web
app's **Labels** page (stored in `vocabulary`, applied as Whisper's
`initial_prompt` on every pass from the next segment — enrolled speaker names are
included automatically). The cheap proper-noun lever; no training involved.

## Golden ASR check

```sh
./scripts/recall.sh score-asr    # transcribe tests/fixtures/speech with the real model
```

Fails if WER on the committed synthetic-speech fixtures (`say`-generated, one per
household language — PII-free by construction) drifts past its threshold, or if
the language is mis-detected. The regression net under the model/decoder seams;
on-demand (loads the model), not part of verify. Regenerate the fixtures only
deliberately: `scripts/gen-speech-fixture.sh`.

## Fine-tune (and prove it helps)

Once corrections accumulate, fine-tune Whisper on the household's voices and
**measure** it before trusting it:

```sh
./scripts/recall.sh finetune-pilot --out /Volumes/Backup/recall
```

Holds out ~20%, trains on the rest, reports base-vs-adapter WER on the held-out
clips — only a held-out win counts. Heavy (~25–45 min), pauses capture+live for
the run. **Deploying a winning adapter is a follow-up** (worker uses mlx-whisper,
the LoRA is HF/PEFT). Method + measured baselines: [pipeline.md §5](pipeline.md).
