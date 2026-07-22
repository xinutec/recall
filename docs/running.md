# Running recall

All ML commands run via `scripts/recall.sh <cmd>` (Nix tools + the `.venv` with
mlx-whisper/pyannote + `HF_TOKEN` from `.env`). Data root is
`/Volumes/Backup/recall` (encrypted), passed as `--out`.

## Services (launchd)

The agents are defined in `deploy/hm-agents.nix` and installed by **home-manager**
— there are no hand-written plists, and no per-agent shell scripts either: each
agent's command lives in that module and is wrapped into the nix store. Apply a
change with: edit that module, commit, then in `~/.config/home-manager` run
`nix flake update recall && home-manager switch --flake .#pippijn`. Restart one
ad-hoc with `launchctl kickstart -k gui/$(id -u)/<name>`. Logs:
`~/Library/Logs/recall/<agent>.{out,err}.log`; health: `./scripts/recall.sh doctor`
(checks every agent is loaded).

**What an agent runs is what was committed.** Its `PYTHONPATH` is the store copy of
the pinned revision, so editing `src/` does not change a running daemon — bump the
lock and switch, as above. The toolchain is unchanged by this: each wrapper still
enters this repo's own devshell, so sox/ffmpeg/python are the versions `flake.lock`
pins. `./scripts/recall.sh <cmd>` still runs the working tree, which is what you
want while developing.

| agent | does | when |
|---|---|---|
| `org.xinutec.recall-capture` | USB mic → gap-free Opus segments | always on |
| `org.xinutec.recall-live` | VAD → transcribe each utterance (~2–3 s, provisional) | always on |
| `org.xinutec.recall-worker` | index + transcribe new segments (whole-clip; diarization is the refine agent's job) | continuous |
| `org.xinutec.recall-api` | Angular web app + JSON API on `:8000` | always on |
| `org.xinutec.recall-ingest` | one TCP server (port 9999) for all phone mics | when phones used |
| `org.xinutec.recall-refine` | re-derive segments diarized + speaker-split; also drains queued A/B model comparisons | diarize: while capture paused · A/B: any time |
| `org.xinutec.recall-llm-host` | holds the LLM (one copy for the whole Mac) and generates on `127.0.0.1:8092` | always on; weights loaded on demand, released after 5 min idle |

The off-machine backup is **odin's**, not the Mac's: odin's nightly restic takes an
integrity-checked SQLite snapshot from inside the Isis pod plus an rsync of the audio
PVC (`nixos-config machines/odin/backup-prepare.sh`), so every recording is protected
server-to-server. The Mac keeps the protected master archive on this volume and pushes
it to Isis (`recall-sync`); it runs no backup agent of its own. The training corpora
(`finetune-corpus`, `pilot-*`) live only here and are deliberately not backed up — they
are derived from the archive + corrections and can be regenerated.

> **macOS mic permission is per-agent:** capture and live each need their own
> grant. If an err log shows `Out:0`, allow the prompt (or System Settings →
> Privacy → Microphone) and `kickstart -k`.

## Web app (`:8000`)

`http://<mac-ip>:8000` (LAN) — timeline, full-text search with playback,
review/correct queue, phone-as-mic recording, and speaker labelling (which enrols
voices as you confirm who spoke).

```sh
./scripts/recall-build-frontend.sh    # rebuild UI; the service serves it live, no restart
nix develop --command bash -c 'cd frontend && npx ng serve'   # dev: hot reload, proxies /api
```

## Capture & verify

USB mic → `/Volumes/Backup/recall/`, gap-free, auto-restarts.

Capture and live both pin the mic with `--device "USB Condenser Microphone"`
(`deploy/hm-agents.nix`). Never record from the
*default* input: macOS re-points it at whatever connects, e.g. a Bluetooth
speaker's hands-free mic — which then chimes into call mode and records at
telephone quality. A renamed/missing device makes sox fail hard and the agent
crash-loop (visible in `logs/capture.err.log`) rather than silently recording
from the wrong mic.

```sh
launchctl bootout   gui/$(id -u)/org.xinutec.recall-capture                                # stop
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.xinutec.recall-capture.plist   # start
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

Neither loads the model itself. The weights live in **`recall-llm-host`**
(`src/recall/llmhost.py`) — one process for the whole Mac, so recall and life's
emotion worker cannot each hold their own ~4.3 GB copy — and everything else is
an HTTP client (`recall.llm.make_generator`). It loads on the first request and
lets go after five idle minutes, so an unused day costs nothing and a first
request after a lull pays ~60s. Nothing falls back to loading in-process: with
the agent down, generation fails loudly (`LlmHostUnavailable`) rather than
quietly re-creating the second copy.

```sh
curl -s localhost:8092/health            # which model is resident, and how idle
./scripts/recall.sh llm-host --idle-unload 60   # run one by hand (agent stopped)
RECALL_LLM_HOST= ./scripts/recall.sh summarize  # deliberate in-process load
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
