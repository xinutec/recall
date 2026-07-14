# Splitting recall: Isis system-of-record + Mac compute worker

Status: **decided and being built, 2026-07-13.** The forks below are resolved (see
"Decisions"). The archive was pruned first — it is now ~700 MB (389 MB audio in 2,252
segments, a 303 MB SQLite DB), so this is a topology change, not a bulk-data move.

> **Where the work is, 2026-07-14.** The one-time seed landed the archive on Isis. Then
> capture control + live sync were built and **deployed through the fleet side only**.
> The next action is the Mac cutover — see **"Capture control over the VPN + live sync"**
> near the bottom for the exact status and the step-C runbook. Start there.

## Decisions (2026-07-13)

1. **Host: Isis.** Not because Isis is the sturdier machine — it is not — but because
   **amun is due to be reinstalled**, and the point of the exercise is to get services
   off amun so it can be wiped without taking them down. Consolidating onto Isis is the
   goal, not a compromise.
2. **At-rest encryption: deferred, and tracked as an open item — not a precondition.**
   The original plan called it one. That was inconsistent: the same audio *already* sits
   in plaintext on odin every night via the existing backup, so encrypting Isis alone
   would protect against a stolen Isis disk while odin keeps plaintext copies of the
   same recordings. It is a real gap, but it is a **fleet-wide** gap and predates this
   migration; blocking the migration on it would buy nothing. It is listed under "Open"
   below and must not be quietly forgotten.
3. **Access: VPN/LAN-only** (unchanged — the stance since the 2026-07-09 rollback).
4. **Database: SQLite on Isis** (only Isis writes; `.backup` for a consistent snapshot).

## Open (do not lose these)

- **At-rest encryption, fleet-wide.** Isis and odin both store this household/medical
  audio on plaintext disk. Fixing Isis alone is half a step. Needs a key-custody answer
  first — a keyfile on the same disk is theatre.

## Goal

Stop the Mac mini being recall's single point of failure. Today it is the capture
device, the compute, the database, the web server, and the backup origin all at once.
The aim is to keep on the Mac only what physically must live there, move the durable
state and the serving onto the fleet, and fold recall into the fleet's standard backup
so it stops being a bespoke, Mac-centric path.

## The hard constraint

recall's ML is Apple-Silicon-bound and cannot move off the Mac usefully:

- `mlx-whisper` (ASR) — Metal/MLX only.
- `pyannote` (diarization, embeddings) — torch; runs on CPU but slowly.
- `mlx-lm` Qwen (day summaries, ask) — Metal/MLX only.

Isis is 4 CPU cores, 15 GB RAM (shared with Nextcloud), **no GPU** (measured
2026-07-11). Running diarization or a 7B LLM there would crawl. So "only audio-to-text
on the Mac" is not the split; the split is **compute vs. state**:

- **Mac** stays the compute node (capture + all ML), but holds **no system of record**.
- **Isis** becomes the durable store and the always-on server, holding only pre-computed
  data. It runs no ML.

If the Mac dies, the loss is bounded to in-flight capture buffers — the archive,
transcripts, and summaries live on Isis (and its backups).

## Topology

```
                          home LAN / WireGuard 10.100.0.0/24
  ┌─────────────── Mac mini (10.100.0.11, one-way WG peer) ───────────────┐
  │  capture (USB mic) → Opus segments                                    │
  │  worker/live: mlx-whisper turbo  (audio → text)                       │
  │  refine: pyannote diarize + speaker align                             │
  │  llm: mlx Qwen (summaries, ask answers)                               │
  │  outbox + poller  ── all traffic Mac-INITIATED ──►                    │
  └───────────────────────────────┬───────────────────────────────────────┘
                                   │  (1) push: audio blobs + transcript/turn/summary rows
                                   │  (2) poll: pending ask-queries, re-diarize requests
                                   ▼
  ┌─────────────── Isis (10.100.0.2, single-node k3s) ────────────────────┐
  │  ingest/queue API   (WG-bound, authenticated)                         │
  │  store: SQLite system-of-record + audio archive  (plaintext — see Open) │
  │  api + web: timeline / search / review / ask      (VPN/LAN-only)      │
  └───────────────────────────────┬───────────────────────────────────────┘
                                   │  odin restic PULLS (SSH)
                                   ▼
  ┌─────────────── odin (backup host, /backup/restic) ────────────────────┐
  │  restic-backups-cluster: backup-prepare.sh + a new recall block       │
  └───────────────────────────────┬───────────────────────────────────────┘
                                   │  Mac restic-copy PULLS (read-only SFTP, Mac-initiated)
                                   ▼
                         Mac off-site copy  (standard fleet pattern)
```

## Security model

The Mac is a **one-way WireGuard peer**: it may initiate into `10.100.0.0/24`; nothing
on the VPN may initiate toward it (enforced by Mac `pf` + per-host iptables — see
`~/Code/nixos-config` and the mac-mini VPN setup). That isolation is the whole reason the
Mac is a safe off-site backup sink, so the design must not weaken it. It does the opposite:
it makes the Mac's isolation the backbone.

**Invert control so the Mac always initiates.** Isis is passive with respect to the Mac:

- Isis holds a **work queue**. The Mac **polls** it for jobs (ask-queries, re-diarize
  requests) and **pushes** results (transcript rows, turns, summaries, audio blobs).
- Isis never connects to the Mac — consistent with the VPN rule, no exception needed.
- Interactive **ask** still works despite the LLM being on the Mac: the web UI on Isis
  enqueues the question; the Mac polls, answers with mlx Qwen, pushes the answer back.
  It is asynchronous (answer appears when the Mac processes it), not a live round-trip.

**Channel.** A small ingest/queue API on Isis, **bound to the WG interface only** — not
the shared k3s ingress. (The ingress answers on Isis's public IP regardless of DNS, i.e.
obscurity, not a firewall; a prior remote-access attempt confirmed this and was rolled
back.) Authenticate the Mac with mTLS or a bearer token sourced from agenix/Vaultwarden,
not committed.

**Encryption at rest — NOT done, and not a blocker (decided 2026-07-13).** Isis has no
LUKS/dm-crypt (re-checked 2026-07-13; neither has amun). This migration therefore puts
household and medical audio on plaintext disk — but so does today's arrangement: the
nightly backup has been copying the same audio to odin in the clear all along. Encrypting
Isis alone would guard a stolen Isis disk while odin kept plaintext copies of the same
recordings, which is a ritual, not a control. The gap is real and **fleet-wide**; it is
tracked under "Open" and needs a key-custody answer (a keyfile on the same disk buys
nothing) rather than a rushed LUKS container. The Mac's `/Volumes/Backup` is encrypted
today and stays so.

## Queue API sketch (WG-bound, token/mTLS)

Illustrative, not final — shape it to the real store methods.

```
POST /v1/segments        # audio blob + metadata (Mac → Isis), idempotent by segment id
POST /v1/turns           # transcript/diarized turns for a segment, supersede-aware
POST /v1/summaries       # day / ask-answer payloads
GET  /v1/jobs?type=ask   # Mac polls: pending ask-queries + re-diarize requests
POST /v1/jobs/{id}/done  # Mac marks a job done with its result
```

All writes are supersede-aware (recall never mutates in place; a better pass supersedes).
The Mac keeps a local **outbox** so a network gap never drops a segment — it retries until
Isis acknowledges, then the local copy is scratch.

## Backup normalization (and it removes today's TCC failure)

Once recall's data lives on Isis, recall joins the fleet's standard backup and stops being
a special case:

1. **odin restic** pulls recall from Isis into `/var/backup-staging`, then snapshots to
   `/backup/restic`. This needs a **new per-app block** in
   `~/Code/nixos-config/machines/odin/backup-prepare.sh` — that file has one hardcoded
   block per app and does not iterate, so recall is a silent gap until added. The block
   takes a consistent DB snapshot (`sqlite3 … .backup`, not a live file copy) plus the
   audio dir.
2. The Mac's existing `restic copy` off-site job pulls from odin over read-only SFTP
   (Mac-initiated) — recall rides along like every other app.

The bespoke `recall-backup.sh` (Mac → `odin:/backup/recall-mirror` rsync) is retired.
Today it is **broken**: the launchd agent's shell tools have no macOS TCC grant for the
external volume, so it has produced no off-site copy since 2026-07-02. Moving the data off
the Mac makes that whole path — and its failure mode — disappear.

## Deploying the Isis side (k3s)

recall's Isis tier is a **k3s workload** on Isis — a `Deployment` (api + web + the sync
ingest, one image) plus **PersistentVolumeClaims** for the SQLite DB and the audio
archive (k3s local-path on Isis's disk; ~5 GB and growing, well within the 1.1 TB free).
Running it in-cluster rather than as a bespoke systemd service is what makes it "as all
other systems" — it's picked up by the same odin-restic backup and Mac restic-copy, no
special path.

- **Backup block** — recall's PVCs join `backup-prepare.sh` on odin as a new per-app
  block. NOT the MariaDB-dump shape the other apps share: SQLite gets a consistent
  `sqlite3 .backup` of the DB PVC (the Nextcloud-redis RDB dump is the precedent), plus
  the audio PVC pulled as-is. Without the block recall is a silent backup gap — that file
  has one hardcoded block per app and does not iterate namespaces.
- **Network gate (critical)** — the sync ingest and the web UI must NOT sit on the shared
  public k3s ingress. That ingress answers on Isis's *public* IP regardless of DNS —
  obscurity, not a firewall (confirmed during the 2026-07-09 remote-access attempt). Bind
  them to the **WireGuard interface only**: a `hostPort`/NodePort pinned to Isis's WG IP
  `10.100.0.2`, or a dedicated ingress listening solely on `wg0`. That is the real
  network-layer gate the Mac dials into.
- **Encryption at rest** — local-path PVCs live under `/var/lib/rancher/k3s/storage` on
  Isis's disk, which is unencrypted today. Encrypt that storage (LUKS) or mount an
  encrypted volume there before recall's audio lands on it.

`recall.sync` is deployment-agnostic — the same FastAPI app runs in a pod or a systemd
unit — so this shapes the manifests (`~/Code/pippijn/code/kubes/`) and odin's
nixos-config, not the Python. It is inert until `RECALL_SYNC_TOKEN` is set.

## Forks — resolved

All five are settled; see "Decisions" at the top. Recorded here so the reasoning is not
lost: **host** (Isis, to free amun for reinstall), **encryption** (deferred, tracked, and
argued above), **access** (VPN/LAN-only), **database** (SQLite), **server location**
(home-lab — the audio already reaches odin there today).

## Migration path (incremental, each step reversible)

1. Stand up the Isis store + ingest/queue API as a **mirror**; the Mac dual-writes.
2. Point the Mac's worker/refine/llm at Isis as the source of truth; verify parity.
3. Cut the web UI over to Isis (VPN/LAN-only).
4. Add the odin `backup-prepare.sh` recall block; **verify a restore** before trusting it.
5. Retire the Mac's local api/store and the bespoke `recall-backup.sh`.

Keep each step behind a flag so any step can roll back to the Mac-only path.

## What explicitly stays on the Mac

Capture (USB mic), ASR (mlx-whisper), diarization/embeddings (pyannote), the LLM
(mlx Qwen), a local outbox for reliable delivery, and the off-site restic copy. Nothing
stateful that another node depends on.

## Capture control over the VPN + live sync (2026-07-14)

The goal Pippijn asked for: **control the mic (pause/resume) from Isis's UI over the VPN,
and keep Isis a live mirror** instead of the frozen one-time seed. Because the Mac is a
one-way WireGuard peer, Isis cannot dial it, so control is **inverted**: Isis holds the
desired state, the Mac polls and applies it.

**What was built** (recall commits `6f53688` waivers, `e5b0f85` feature, `391be8d` agents;
monorepo `06f226ce` sets `RECALL_ROLE=fleet`):

- **Capture intent on the fleet.** `RECALL_ROLE=fleet` makes `/api/capture/pause|resume`
  record *intent* in the store (`capture_control.intent_*`) instead of writing a local
  pause file nothing reads. The role is explicit because the Mac also sets
  `RECALL_SYNC_TOKEN`, so the token can't tell the roles apart.
- **The Mac mirrors it** (`recall.capture_mirror`): a `POST /sync/capture` handshake
  reports the Mac's applied state and pulls the intent in one round trip; the Mac writes
  its local `capture_paused_until` to match. **Short-poll every ~5s** (matches the capture
  agents' own 5s self-gate; SSE noted as a later upgrade if that gate is tightened).
  **Edge-triggered** so it doesn't clobber a pause set on the Mac's own LAN UI. The
  fleet's `/api/capture` status shows the Mac's *reported reality*, falling back to intent
  when the Mac goes quiet (a pause you can't confirm is worthless).
- **Live data sync** reuses the existing `sync_push` on a 2-minute timer.
- Two launchd agents in `deploy/hm-agents.nix`: `recall-sync` (timer, data) and
  `recall-capture-mirror` (resident 5s loop). Both read `RECALL_SYNC_TOKEN` from `.env`.

**Deploy state:**

- **A — image:** done. CI published `xinutec/recall:latest` with the code.
- **B — Isis rolled out:** done and verified over VPN. `RECALL_ROLE=fleet` live;
  `pause`→bounded intent, `resume`→clear, both correct from the Mac's WG IP; data serving
  intact (2,252 segments). Intent left in the **running** state.
- **C — the Mac cutover: NOT done. This is the next action.** Until C, the fleet is ready
  but disconnected: pressing pause on Isis records intent that nothing applies, and Isis
  is still the frozen seed (the Mac isn't pushing). The Mac's local LAN UI still controls
  the mic directly, unaffected.

### Step C — the Mac cutover (runbook)

Do these on the **Mac** (this repo). The token must go from the cluster into a file
without passing through logs/output.

1. **Token into `.env`** (never echo it):
   ```sh
   TOKEN=$(ssh root@10.100.0.2 \
     'kubectl -n recall get secret recall-secret -o jsonpath="{.data.SYNC_TOKEN}" | base64 -d')
   grep -q '^RECALL_SYNC_TOKEN=' ~/Code/recall/.env \
     || printf 'RECALL_SYNC_TOKEN=%s\n' "$TOKEN" >> ~/Code/recall/.env
   unset TOKEN
   grep -c '^RECALL_SYNC_TOKEN=' ~/Code/recall/.env   # expect 1; never print the value
   ```
2. **Dry-run both commands by hand** (recall.sh sources `.env`):
   ```sh
   cd ~/Code/recall
   ./scripts/recall.sh sync --url http://10.100.0.2:8000 --out /Volumes/Backup/recall
   #   expect: "sync: pushed N segment(s) to http://10.100.0.2:8000"
   ./scripts/recall.sh capture-mirror --url http://10.100.0.2:8000 --out /Volumes/Backup/recall
   #   expect: "capture-mirror: no change"   (fleet intent is running)
   ```
3. **Install the agents:**
   ```sh
   cd ~/.config/home-manager && nix flake update recall && home-manager switch --flake .#pippijn
   ```
4. **Post-switch checks (CRITICAL — this reloads the live mic agents):**
   - Both new agents loaded: `launchctl list | grep -E 'recall-(sync|capture-mirror)'`.
   - **Mic-TCC survived:** `recall-capture` and `recall-live` are loaded with **last exit
     0**, not crash-looping (a TCC block crash-loops instead of running). Check their
     `logs/{capture,live}.err.log` for mic-permission errors. If they crash-loop, the mic
     grant was lost → **roll back** to the previous home-manager generation and stop; do
     not leave capture down (this is the medical recorder).
5. **End-to-end (step D):**
   - Pause on Isis (`curl -X POST http://10.100.0.2:8000/api/capture/pause` or the UI):
     within ~5–10s the Mac grows `/Volumes/Backup/recall/capture_paused_until` and capture
     parks. **Resume** clears it and capture restarts. Then **resume** so the mic is on.
   - Live sync: after ~2 min, Isis `/api/status` segment count tracks new Mac recordings.

Then the migration's "mic control through Isis" goal is met. Remaining separately-tracked
items: retire the Mac's local api/UI (migration path step 5), the deferred fleet-wide
at-rest encryption (see "Open"), and the pre-existing frontend dev-lint debt waived in
`6f53688` (Pippijn is improving those checks).
