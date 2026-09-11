# Running recall

All ML commands run via `scripts/recall.sh <cmd>` (Nix tools + the `.venv` with
mlx-whisper/pyannote + `HF_TOKEN` from `.env`). Data root is
`/Volumes/Backup/recall` (encrypted), passed as `--out`.

`.venv` is a symlink into the nix store (`nix build .#dev-env --out-link .venv`),
so a person runs the same interpreter the agents do rather than a second copy
built by hand. If it is missing, that command is how it comes back.

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
| `org.xinutec.recall-ingest` | one TCP server (port 9999) for all phone mics | when phones used |
| `org.xinutec.recall-beat-relay` | accept a mic app's heartbeat on the LAN (port 8000) and forward it to Isis, for a phone whose VPN is down | always on |
| `org.xinutec.recall-refine` | re-derive segments diarized + speaker-split | while capture paused |
| `org.xinutec.recall-llm-host` | holds the LLM for the whole Mac on `127.0.0.1:8092` — kept for *life*, not for recall (see below) | always on; weights loaded on demand, released after 5 min idle |
| `org.xinutec.recall-sync` | push the archive to Isis (the system of record) — only what changed since the last watermark | timer |
| `org.xinutec.recall-upload` | store-and-forward delivery: closed segments → recalld on Isis, sha-256 receipts re-hashed before anything counts as delivered ([architecture.md](architecture.md) stage B) | timer |
| `org.xinutec.recall-capture-mirror` | poll Isis's desired capture state and mirror it onto the local pause file | every ~5 s |
| `org.xinutec.recall-jobs` | pull Isis-queued work (refine, upload) into the Mac's local queues | timer |
| `org.xinutec.recall-doctor` | run the health checks and report them to fleetwatch (Rust, `doctor/`) | every 5 min |
| `org.xinutec.recall-speech` | measure how much of each archived segment is SPEECH — the evidence the quiet review needs before it may propose deleting anything (Rust, `audiod speech`) | every 5 min |

There is deliberately **no `recall-api` agent**: the Mac serves no UI or control plane
(see the Isis split below). `recall-sync`, `recall-jobs` and
`recall-capture-mirror` are inert until `RECALL_SYNC_TOKEN` is set; the other
credential-carrying agents use their own — `recall-upload` takes
`RECALL_INGEST_TOKEN`, `recall-doctor` the fleetwatch token, and `recall-speech`
needs none. Named rather than counted: the table's order is not a contract.

### The doctor runs itself twice, and that is on purpose

`doctor` starts a child — itself, with `--collect` — that does every read of
`/Volumes/Backup` and prints its checks as JSON, and a parent that reports them
while touching only launchd and `~/.config`. If the child does not answer within
60 seconds the parent **abandons it** and reports
`archive/archive answers: no answer in 60s`, naming the abandoned pid on stderr.

So a stray `doctor --collect` in `ps`, in `U` state, is the system working:
a process in uninterruptible disk wait cannot be killed until its I/O completes,
so leaving it is the only way for the doctor to come back at all. It exits by
itself when the volume does. Do not go hunting it — go and find what owns the
disk queue.

### Speech is measured HERE, not on Isis

`recall-speech` decodes every archived segment once and records how many seconds
of it are speech. Two consumers need that: the quiet review may not propose
deleting a segment without it, and it is the only signal that tells a broken
microphone from a quiet room.

⚠ **It runs on the Mac deliberately**, though recalld measures the same thing on
Isis. Removing audio from this archive is a Mac-local act
([architecture.md](architecture.md), "Deletion authority"), and a guard that had
to fetch its evidence over the network would either block cleanup whenever the
fleet is unreachable or — worse — proceed without it. The fleet also cannot
answer for everything: measured 2026-09-08, 730 of 14,777 segments had never
been delivered, and those are the least replicated audio in the house.

⚠ **It is the SAME detector, not an equivalent one** (`audiocore::vad`, shared
with recalld). Two detectors disagreeing about what counts as speech is not a
discrepancy to reconcile later: one of the two answers is a licence to delete
audio somebody was talking in.

⚠ **A pass that fails ENTIRELY writes nothing.** `speech_s` is never revisited
once set, so a bad pass is permanent — and the two ways it went wrong while
being built were both this process, not the audio: segments ffmpeg had not
finished writing, and ffmpeg missing from PATH altogether. A broken file among
good ones is believable; every file broken is the instrument.

### The worker leaves a pulse

`<data root>/worker-heartbeat.json` is stamped at the start and again at the end
of every worker pass, and the doctor grades its age as `capture/worker pulse`
(warn 30 min, fail 1 h — set from the **cold** first pass, measured at 513 s
because it loads the speaker-ID models off the spinning disk; steady-state empty
passes are 17–22 s, and an empty pass is not a fast one since the bounded
backfills run regardless). The worker's *log* cannot answer this: it prints only
when a pass writes transcript rows, so a quiet house and a wedged pipeline both
leave an empty `worker.out.log` — on 2026-08-10 that log was three days old while
an hour of captured audio went unindexed.

A pass stamped as started but never finished says `a pass has been running N min`,
which is a different fault from `last pass N min ago` even though the clock is the
same one: the first points at the archive, the second at launchd. A pass that
raises never stamps a finish, so a crash-looping worker reads as the former.

The reason is 2026-08-10: a bulk delete on that volume starved every reader for
over an hour, the doctor wedged along with the worker, refine and sync, and
because launchd will not start a new run while the old one is stuck
(`KeepAlive = false`, `StartInterval = 300`), *every* doctor after it was
silenced too. The outside view of the same fault is fleetwatch's `volume-latency`
collector on this machine (`xinutec-infra/mac-mini/volume_latency.py`); this is
the inside one.

The off-machine backup is **odin's**, not the Mac's: odin's nightly restic takes an
integrity-checked SQLite snapshot from inside the Isis pod plus an rsync of the audio
PVC (`nixos-config machines/odin/backup-prepare.sh`), so every recording is protected
server-to-server. The Mac keeps the protected master archive on this volume and pushes
it to Isis (`recall-sync`); it runs no backup agent of its own. The training corpora
(`finetune-corpus`, `pilot-*`) live only here and are deliberately not backed up.
⚠ They can no longer be REGENERATED, which is why that is no longer the reason:
the toolchain that made them was deleted when training was cut. They are
leftovers of it, kept only because deleting data is a deliberate act.

> **Only capture needs the mic grant.** `live` never opens the device — it
> subscribes to the UDP tap capture publishes, and its `--device` argument is
> vestigial (`live.py`, `sources.live_input_argv`), because two CoreAudio
> clients on one device starve each other.
>
> ⚠ **A denied grant is DIGITAL SILENCE, not an error.** The agent starts, sox
> runs, segments are written, and every one of them is silent — measured
> 2026-09-10, a granted mic read mean −47.7 dB where a denied one reads −inf.
> So nothing appears in any err log: look at the LEVELS, never for a message —
> `doctor --out <archive root> --collect` prints them as JSON. Grant it in System
> Settings → Privacy → Microphone, then `kickstart -k`.
>
> ⚠ **Do NOT read `mean_volume` off `audio_segments`: it is WRITE-DEAD.** Measured
> 2026-09-11 — zero of September's segments carry one, and the last real
> `envelope` was written 2026-07-12T15:50:27, when D2 moved level measurement into
> recalld's scanner and both columns were left behind. A NULL there reads exactly
> like a quiet mic, which is the failure this paragraph exists to catch. The live
> evidence is `segment_levels` in recalld's `ingest.sqlite`.

## Web app (Isis, `:8000`)

**`http://10.100.0.2:8000`** — on Isis, over the VPN, behind a Nextcloud sign-in.
Timeline, full-text search with playback, review/correct queue, phone-as-mic
recording, and speaker labelling (which enrols voices as you confirm who spoke).
The Mac serves nothing: it is capture + ML + push, and `:8000` there refuses.

Shipping a UI change means shipping the image: the Dockerfile builds the Angular app,
so push to `main` (CI builds `xinutec/recall:latest`) then roll Isis with
`ssh root@10.100.0.2 'kubectl -n recall rollout restart deployment/recall'`.

```sh
./scripts/recall-build-frontend.sh    # build into dist/ (what the image does; also for a local check)
nix develop --command bash -c 'cd frontend && pnpm start'     # dev: hot reload, proxies /api
```

## Capture & verify

USB mic → `/Volumes/Backup/recall/`, gap-free, auto-restarts.

Capture pins the mic with `--device "USB Condenser Microphone"`
(`deploy/hm-agents.nix`); live takes no device at all. Never record from the
*default* input: macOS re-points it at whatever connects, e.g. a Bluetooth
speaker's hands-free mic — which then chimes into call mode and records at
telephone quality. A renamed/missing device makes sox fail hard and the agent
crash-loop (visible in `~/Library/Logs/recall/capture.err.log`) rather than silently recording
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

## runner + shims (stage E3, SHADOW — nothing reads its output yet)

The Rust `runner` is the Mac's whole job orchestration in the target
architecture: lease a job from recalld, fetch the blob, drive a model shim over
stdio, push the result, ack. It holds no state — no watermark, no outbox — so
killing it costs an expiring lease and nothing else.

⚠ **It runs BESIDE the worker above, not instead of it**, and a transcript you
read is still the worker's. The interpreter and the write path now exist
(`room_turns`), but the writer is OFF: switched on 2026-09-12 and switched off the
same evening, because its turns measured WORSE than the per-mic ones they hid —
22% repetition loops against 0%, and a Dutch household reported as mostly English.
What it wrote was reversed.

It does NOT wait on #1461. The open question is whether #1410's read-path filters
close that gap, or whether the room audio is genuinely worse — see #1388.

```sh
# one job, then stop — the shape to use when checking it by hand
RECALL_SYNC_TOKEN=… PYTHONPATH=src ./target/release/runner \
  --url http://10.100.0.2:8001 --api http://10.100.0.2:8000 --once \
  --shim .venv/bin/python -m recall.shim_asr

# drop --once to poll continuously
```

It reads the household vocabulary from `--api` at startup and passes it as
Whisper's `initial_prompt` on every job. ⚠ **If that read fails the runner
exits** rather than transcribing unbiased: a corpus without the biasing it was
built for has to be redone (#1463).

A shim is drivable by hand, which is the fastest way to tell a model problem
from a protocol one:

```sh
echo '{"id":"1","op":"transcribe","audio":"tests/fixtures/speech/public-domain-en.flac"}' \
  | PYTHONPATH=src .venv/bin/python -m recall.shim_asr
```

⚠ **Its stdout is the protocol.** Model chatter goes to stderr on purpose; a
stray line on stdout would desync the stream silently.

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

## llm-host — the model holder (recall no longer asks it anything)

**Ask and day summaries were CUT on 2026-09-06** with the product's scope
([architecture.md](architecture.md), "Scope of the rebuilt product"), so recall
generates no text of its own any more.

⚠ **The holder itself STAYS, and deleting it would break a different project.**
`recall-llm-host` (`src/recall/llmhost.py`, `127.0.0.1:8092`) is the one process
on this Mac that owns the ~4.3 GB of LLM weights, and *life*'s emotion worker
addresses it directly over loopback. It is recall's agent by history, not by
ownership.

```sh
curl -s localhost:8092/health            # which model is resident, and how idle
./scripts/recall.sh llm-host --idle-unload 60   # run one by hand (agent stopped)
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

Fails if WER drifts past a fixture's threshold, or if the language is
mis-detected. The regression net under the model/decoder seams; on-demand (loads
the model), not part of verify.

All three fixtures are committed, so it scores three on any clone — including
the Dutch one. `public-domain-en.flac` is a licence-clean reading; the
`dialogue-{en,nl}` pair is macOS `say` reading invented lines, regenerable with
`scripts/gen-speech-fixture.sh`. The pair was absent from a clone until
2026-09-09, when the blanket `*.flac` ignore that swallowed it was narrowed
(#1433) — that is why the code still speaks of fixtures that may be missing. A
missing one now FAILS rather than being skipped, and a run that finds none fails
rather than reporting success on nothing.

Thresholds are per fixture and set from each one's own measured baseline (see the
table in `cli.py`), so they have equal detection power rather than an equal
number. Regenerate the fixtures only deliberately — a new `say` voice moves the
baseline underneath the threshold.

## Fine-tuning — CUT

Training was dropped on 2026-09-06 (architecture.md, "Training is not a goal"),
and `finetune`, `finetune-pilot` and `export-training` no longer exist. What is
kept is **enrolment**, which is not training: labelling a voice attaches a name
to a voiceprint, and that is how attribution works. Corrections are still
collected — see "Enrol the household" above.
