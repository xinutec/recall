# Splitting recall: Isis system-of-record + Mac compute worker (PROPOSED)

Status: **proposal, 2026-07-11.** Nothing here is built. The open decisions in
"Forks" are unresolved; resolve them before implementing.

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
  │  store: SQLite system-of-record + audio archive  (encrypted dataset)  │
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

**Encryption at rest.** Isis's disk shows no LUKS/dm-crypt (checked 2026-07-11), and this
moves sensitive household/medical audio onto it. Provision an **encrypted dataset** for
recall on Isis before any audio lands there, and confirm odin's repo posture too. The
Mac's `/Volumes/Backup` is encrypted today; parity on Isis is a precondition, not a
follow-up. (Note: audio already replicates to odin under the current backup, so Isis
storing it is not a new class of exposure — but the at-rest gap is real.)

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

## Forks (decide before building)

1. **At-rest encryption on Isis** — recommended: required precondition (LUKS/dm-crypt
   dataset for recall). Not optional given the audio's sensitivity.
2. **Database** — recommended: keep **SQLite** on Isis (only Isis writes now, so the
   single-writer model holds; back it up via `.backup`). Alternative: port to MariaDB for
   a fleet-uniform dump block — larger rewrite (migrations, FTS5), defer.
3. **Host** — you named **Isis**, but it is the fragile host (a bad upgrade once forced
   the amun/isis split). recall's DB+API+audio is light and fits, provided no ML runs
   there. Weigh **amun** (more robust) as the alternative.
4. **Access** — recommended: **VPN/LAN-only** (the current stance since the 2026-07-09
   rollback). The rolled-back `oauth2-proxy` + ACME DNS-01 design for a public
   `recall.xinutec.org` is sound and can be revived later.
5. **Server location / privacy** — confirm the fleet is home-lab, not colo, for the
   household-audio privacy model. (Audio already reaches odin today either way.)

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
