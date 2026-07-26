# The Mac/Isis split: Isis system-of-record + Mac compute worker

Status: **live since 2026-07-17.** This is the architecture of the running system —
why the split exists, how control is inverted across the one-way VPN, and where the
security boundaries are. The one-shot migration procedure has been removed now that
it is spent; `git log` has it if you ever need the archaeology.

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
5. **Phone ingest stays on the Mac/LAN (2026-07-16).** The phones' PCM must reach the
   Mac regardless — the live MLX pass and the master archive live there — and Isis
   cannot dial the Mac, so Isis-side ingest would need a Mac-initiated pull-stream
   re-architecture. It would buy exactly one thing (phone capture surviving a Mac
   outage), and a Mac outage is already visible (liveness dots, doctor), not silent.
   All capture, USB and phones alike, is the Mac's job.

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

### Web-UI sign-in via Nextcloud SSO (2026-07-17)

The WG binding is the network gate; on top of it the **human web UI** is gated behind a
Nextcloud sign-in (`dash.xinutec.org`), so reaching recall over the VPN isn't enough — you
also have to be a signed-in, allowlisted user. This mirrors health-sync's "Sign in with
Nextcloud" wall. It lives in `recall.webauth` and is **inert unless configured**, exactly
like the sync token: with `RECALL_SESSION_SECRET` + `NC_CLIENT_ID` + `NC_CLIENT_SECRET`
unset, recall is an open LAN UI (the Mac's local UI, dev, and tests are untouched); the
Isis pod sets them (from `recall-secret`) and raises the gate.

**Two planes, deliberately split** — because the recording side is driven by devices and
daemons that cannot do an interactive OAuth login:

- **Browsing plane (gated).** The Angular SPA and its read/write `/api/*` routes require a
  valid session; without one they return `401 {"error": "not authenticated"}` and the SPA
  shows the sign-in wall. A username allowlist (`RECALL_ALLOWED_USERS`, default `pippijn`;
  empty = any dash user) restricts who may enter even after a valid Nextcloud sign-in —
  recall holds household + medical audio, so it is single-user by default.
- **Recording plane (login-free, network-gated only).** `/sync/*` keeps its own bearer
  token (untouched). The iOS mic app talks to Isis's `/api/capture` (long-poll pause
  state), `/api/sources` (fleet liveness), and — by explicit choice — `/api/capture/pause`
  and `/api/capture/resume`, so the phone keeps its pause button without a login. These
  stay reachable by anything on WG/LAN, which is the same trust boundary they had before.

**Mechanics.** OAuth authorization-code flow against Nextcloud's `apps/oauth2`, identity
only: the access token is used once to read the user (`ocs/v2.php/cloud/user`) and then
discarded — there is no local user store. Identity rides a **stateless, HMAC-signed
session cookie** (7-day TTL), so it needs no server-side session store and survives pod
restarts and the read-only rootfs. The OAuth `state` is likewise a short-TTL signed token.
The cookie is **not** `Secure`: recall answers over plain http on the wg0 hostPort
(`10.100.0.2:8000`), and the network remains the real gate — revisit if it ever gains an
https origin. Redirect URI: `http://10.100.0.2:8000/auth/callback`, which must match both
the client registered on dash and the address the browser actually loads recall at.

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
  its local `capture_paused_until` to match. Originally short-poll every ~5s; since
  2026-07-16 the same exchange **long-polls** — Isis holds the reply until intent
  actually changes, so a press reaches the mic in ~RTT while reports still ship every
  ~5s (the hang is the pacing). Still every-connection-Mac-initiated, still degrades
  to the plain short-poll against an older server.
  **Edge-triggered** so it doesn't clobber a pause set on the Mac's own LAN UI. The
  fleet's `/api/capture` status shows the Mac's *reported reality*, falling back to intent
  when the Mac goes quiet (a pause you can't confirm is worthless).
- **Live data sync** reuses the existing `sync_push` on a 2-minute timer.
- Two launchd agents in `deploy/hm-agents.nix`: `recall-sync` (timer, data) and
  `recall-capture-mirror` (resident 5s loop). Both read `RECALL_SYNC_TOKEN` from `.env`.


### Mac UI retired + local break-glass control (2026-07-15)

With Phase 1 live-sync deployed, Isis serves the full timeline (archive + the instant
live feed), so the Mac's own web UI became pure duplication. **Retired it:** dropped the
`recall-api` launchd agent from `deploy/hm-agents.nix` (the Mac now serves nothing —
`:8000` refuses locally; Isis `10.100.0.2:8000` is the sole UI + control plane). Nothing
in the Mac's pipeline needed it: `recall-sync` pushes Mac→Isis, `doctor` reads the DB and
posts to fleetwatch, `capture-mirror` polls Isis, `backup` rsyncs to odin.

Retiring the UI removed the Mac's only *local* control surface, which made Isis a **single
point of failure for pause/resume**: exactly the gap that bit us during a rollout, when the
mirror couldn't pull the resume intent while Isis was unreachable and capture had to be
resumed by hand. Closed it with a **network-free break-glass CLI** — `recall pause
[--minutes N]` / `recall resume` (`cli._cmd_pause`/`_cmd_resume`) write the same bounded
`capture_paused_until` file the capture agents self-gate on, with zero network dependency.
Isis stays the authority when reachable: `capture-mirror` is edge-triggered, so it leaves a
local pause alone until Isis's *intent* actually changes. Intent lives in Isis's DB on the
`recall-data-pvc`, so a pod rollout preserves a deliberate pause (no unwanted resume).

Run break-glass from the Mac: `~/Code/recall/scripts/recall.sh pause` (or `resume`). No
launchd agent, no deploy — the wrapper runs live `src`.

**MLX job-pull — refine done (2026-07-15).** Interactive MLX endpoints are unreachable
from Isis under the one-way WireGuard model (Isis can't dial the Mac), so they need a
Mac-initiated **job-pull** — the same inversion capture-mirror uses. The refine path is
now closed: the `/sync/jobs` + `/sync/jobs/{id}/done` endpoints and `SyncClient.poll_jobs`
/`mark_done` already existed but had no Mac-side consumer; added `recall.jobs.run_jobs_once`
(the `recall jobs` command + a 60s `recall-jobs` launchd timer) that pulls Isis's refine
queue into the Mac's *local* refine queue. It runs no ML itself — the existing idle-gated
refine daemon drains the local queue (never during recording) and the refined turns sync
back through the normal segment/turn push. So a refine requested from Isis's UI now
reaches the Mac. Commit `e29fa43`; TDD `tests/test_jobs.py`.

**Share-upload job-pull — done (2026-07-16).** A session uploaded to Isis's
`/api/sessions` used to sit there recorded-but-never-transcribed: the blob and its rows
landed on Isis, where no worker runs. Closed with the same job-pull inversion, and with
**no queue to maintain**: a pending upload job is *derived* — any UPLOAD-kind segment
whose `transcribed_utc` is unset (nothing on Isis ever sets it) — so an upload can't be
lost to a missed enqueue. `/sync/jobs` now serves them as `type="upload"` alongside
refines (blob filename, title, and stream shape in the job), the Mac's `recall jobs`
timer fetches the blob via the new `GET /sync/audio/file` into its own archive root and
registers the source + segment keyed to the fleet's exact start — so the worker's normal
pass transcribes it, the refine daemon diarizes it, and the pushed-back turns dedupe onto
Isis's existing row (UNIQUE source_id+start). The job retires on the Mac's typed
acknowledgement (`/sync/jobs/{id}/done?type=upload` → `mark_transcribed`), with a belt:
any segment push now marks the fleet row transcribed too. Pre-split sessions self-heal —
their blobs and rows are already on the Mac, so the first pass skips straight to the
acknowledgement. Still to confirm by hand: the phone's share-to-Recall flow points at
Isis.

**Ab-compare job-pull — done (2026-07-16).** Unlike refine, an A/B run's result is the
report row itself (result_json + WER summary), so turn-sync can't carry it back — it
needed its own relay. `/sync/jobs` now serves unfinished runs (queued AND running) as
`type="ab-compare"`; the Mac's jobs pass mirrors each into its local `ab_compare_runs`
queue stamped with the run's fleet id (migration: `fleet_id` column), where the existing
refine daemon executes it. Later passes relay the lifecycle back: "running" once the
daemon starts (`POST /sync/ab-compare/{id}/running`, sent only while the fleet still
says queued), then the report or error (`POST …/result`) — the landing is what retires
the run, never an acknowledgement, so a Mac that loses its local mirror simply re-adopts
the still-served run. Model paths had to become machine-independent too: the API default
for model B is now the bare name `adapter-current` (was an absolute path under the
server's own data root, meaningless across the split), resolved against the local data
root at run time (`cli._resolve_model`).

**Ask-the-archive relay — done (2026-07-18).** The web UI's "Ask" runs a local MLX LLM,
which Isis lacks (`import mlx_lm` → `ModuleNotFoundError` on the x86 pod), so the old
`POST /api/ask` 500'd there and the UI showed "Could not get an answer". Split like every
other MLX job: retrieval + prompt-building stay on the fleet (its store + FTS own the
turn ids), generation is the only MLX step and moves to the Mac. `POST /api/ask` on the
fleet retrieves, builds the grounded prompt, queues an `ask_requests` row (migration
`v41`; `sources` = the cited fleet turn ids, `prompt` self-contained) and returns a poll
id; `GET /api/ask/{id}` resolves once the answer lands. `/sync/jobs` serves ask jobs
**first** (a human is waiting), carrying the prompt; the Mac's `recall jobs` runner adopts
each into a local `ask_requests` copy (never generating — it is a 60s one-shot and must
not load a model), the **refine daemon's** resident model generates it, and the answer
posts back via `POST /sync/ask/{id}/result`, retiring the job. On the Mac's own LAN UI
`/api/ask` still answers inline (MLX is local). Three hardenings, each from a bug that
bit in testing: (1) the relay checks an adopted row's prompt still matches the job before
relaying, so a **reused** fleet id (ids are plain `INTEGER PRIMARY KEY` and free on
delete) can't leak a stale answer — a test "pong" once surfaced for a real question; (2)
the refine loop `store.rollback()`s at the top of each pass, because a write that failed
under lock contention left an aborted transaction open that froze the connection's WAL
read-snapshot, so the idle daemon stopped seeing queued asks and hung — plus recover-and-
continue on any pass error; (3) an ask-save under a busy DB **defers and retries** instead
of turning a transient lock into a terminal error, and `GET /api/ask/{id}` reports a
timeout after 10 min so the UI never spins forever. Commits `dfc61af`→`b35e1e9`; TDD
`tests/test_{store,api,sync,jobs}.py`.

**Mirror completion + deletion tombstones — done (2026-07-16).** Two mirror gaps,
found by asking "does everything the Mac records reach Isis, and does a deletion
confirmed on Isis reach the Mac?":

- *Creation was speech-only.* The turn-watermark push only crossed segments that
  produced machine turns; a speechless minute minted no turn ids, never synced, and
  Isis's quiet review could never see or sweep it (it piled up on the Mac for ever).
  Fixed with a **mirror-completion queue**: every processed segment gets a
  `pushed_utc` stamp when it reaches the fleet (new column), and the sync pass pushes
  anything processed-but-unstamped — with whatever turns it has, often none — capped
  at 500/pass. Old rows are NULL, so the first passes reconcile the whole historical
  archive idempotently (the fleet no-ops what it already holds).
- *Deletion crossed nowhere.* The quiet review lives on Isis now, so a confirmed
  sweep deleted only Isis's copy — the Mac's master archive kept it, and a later push
  could even resurrect it on Isis. Fixed with **tombstones**: every hard delete
  (`delete_audio_segments`, `delete_source`) journals the segment identity
  (source + start, the same key the sync dedupes on) into `deleted_segments`, inside
  the same transaction. `/sync/jobs` serves them as `type="sweep"`; the Mac applies
  the identical deletion to its archive and acks (`done?type=sweep`). The tombstone
  also **vetoes re-ingestion**: a push for a tombstoned identity is refused
  (`SegmentStoredOut.tombstoned`), any racing blob is cleaned up, and the Mac stamps
  it pushed so it never retries — this also fixes the old resurrect-a-deleted-session
  quirk. The invariant is now simple: *a processed segment exists on both machines or
  on neither*, and the doctor asserts it (`mirror_check`: unmirrored-beyond-slack =
  FAIL, same class as a stalled backup).

**Sweep is a request, not an order — the Mac protects itself from Isis (2026-07-16).**
The one-way VPN exists so a compromised Isis cannot reach the Mac; the deletion pull
above quietly broke that — an attacker with the sync token (or Isis itself) could
write tombstones and the Mac would obediently hard-delete its master archive. Fixed by
making a sweep *conditional on the Mac's own evidence*: `_apply_sweep` reads
`sweep_evidence` (the segment's kind, the Mac's own VAD `speech_s`, and whether a
visible turn survives) and honours the tombstone only when all three say speechless
idle capture — the same bar the local quiet review clears before deleting. Any other
tombstone is **refused**: the audio is kept, the refusal journaled in `sweep_refusals`,
and the doctor surfaces the count (`sweep_refusal_check`: >0 = WARN, the alarm that the
guard fired — never FAIL, because nothing was lost). The job is still acked either way,
so there's no re-serve loop; Isis's own tombstone stops the kept segment being
re-mirrored. Consequence, blessed: deleting a *speech-bearing* session from Isis's UI
no longer cascades to the Mac (Isis drops its copy and the tombstone blocks re-sync,
but the Mac keeps its master copy) — deliberate removal of real speech is now a
Mac-local act, which is exactly what "protected master archive" means. The worst a
hostile Isis can command is the deletion of audio the Mac already measured as an empty
room, with odin's restic history behind even that.

(The capture-mirror transport was upgraded to long-poll 2026-07-16 — intent in ~RTT —
so the "replace the transport" follow-up is closed; the break-glass CLI still covers an
unreachable Isis. The one manual check left: the phone's share-to-Recall flow points at
Isis.)

**Speaker attribution has to travel both ways (2026-07-17).** The split severed it in
both directions, because each machine owned half and shared neither. The Mac owns the ML
(it computes each turn's voiceprint `speaker_guess`); Isis owns the UI (a person names a
voice there, writing `speaker_label`). But the Mac→fleet push (`TurnIn`) carried only
the diarization `speaker_cluster` — not the guess — so freshly-pushed audio read
*unknown* on Isis even when the Mac had already guessed the voice; and naming a voice on
Isis wrote `speaker_label` only on the fleet, so the Mac's master archive never learned
the name and, worse, voiceprint **enrolment went silent** — `turns_needing_voiceprint`
keys on `speaker_label`, which stopped being set on the Mac the day the UI moved (no new
voice enrolled after ~2026-07-10). Two additions, each following an existing grain:
- **Guess rides the push.** `TurnIn` now carries `speaker_guess`/`speaker_score`; the
  fleet stores them (`set_speaker_guess` after the insert, since the guess is a
  separate ML pass, not an `add_transcript_segment` column). Isis shows what the Mac
  computed; the Mac has no reason to recompute on the fleet (it has no ML there).
- **Names ride back.** `GET /sync/labels` publishes the fleet's human voice-namings as
  `(source_id, cluster, name)` — the cluster is the key both machines already share. The
  Mac's sync pass pulls the whole set and replays `name_voice` for the diffs
  (`pull_labels`), which lands the names in the master archive and re-feeds enrolment on
  the next backfill pass. Idempotent; an older fleet without the endpoint 404s and the
  pull is skipped (the push half already ran), so deploy order doesn't matter.

Authority is split the way the machines are: the fleet is authoritative for human input
(it is the only UI), the Mac for ML output. A pulled label is applied as given — unlike
a *deletion*, there is no independent local evidence to check a name against, and a name
is reversible (rename; `prune_stale_voiceprints` re-derives enrolment) where a delete is
not. So the sweep veto stays the security boundary; labels are trusted metadata.

