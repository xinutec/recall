# Target architecture: store-and-forward, one room stream, Rust on the server

**Status: decided 2026-09-05, being built.** This file replaces the
store-and-forward questions doc (git history has it); every question it raised
is answered in the decision record below. [isis-migration.md](isis-migration.md)
describes the system as it **runs today** — read this file as the destination
and the ladder to it, not as what exists. The migration policy of
[design.md §9](design.md) governs the whole ladder: a Python path is deleted
only after its Rust replacement has survived real days.

## Why this shape

Three measured facts force it; none of them is a preference.

- **The ML is Apple-Silicon-bound and nothing else is.** mlx-whisper and mlx-lm
  are Metal-only; pyannote crawls on CPU ([isis-migration.md](isis-migration.md),
  "the hard constraint"). Everything that is not a model call — recording,
  delivery, storage, alignment, selection, VAD, the queue, the web backend — is
  invariant-heavy plumbing, exactly the half [design.md §9](design.md) already
  assigns to Rust when touched. This redesign touches all of it.
- **Combination lost; selection tied.** SNR-weighted fusion failed its WER gate
  and is null even between equal microphones; calibrated per-block *selection*
  reproduces the best microphone exactly ([audio-plane.md](audio-plane.md),
  "What the gate measured"). Selection needs no phase and near-zero CPU — so
  the room stream can be produced on Isis, and the Mac shrinks to a stateless
  GPU worker.
- **Streaming PCM discards on disconnect, by design** ([devices.md](devices.md)):
  the server rebases a connection by one offset measured at its first byte, so
  a replayed backlog would drift. Requirement #1 is completeness; the fix named
  there — *a protocol that times each segment, not a bigger buffer* — is this
  architecture.

## The shape

```
phones (Kotlin/Swift)      geb + machines (audiod)      Mac USB mic (audiod)
   each records CLOSED segments locally, capture-stamped, cached on device
        └─────────────┬── PUT segment, sha-256 receipt ──┬─────────┘
                      ▼                                  ▼
 ┌─ Isis — recalld (Rust): the system of record ──────────────────────────┐
 │  ingest plane: append-only blob store + ingest.sqlite   (no delete     │
 │  VAD at ingest → speech evidence, liveness               endpoint      │
 │  room builder: tier-1 align + calibrated selection       exists)       │
 │  work queue → jobs out, results in                                     │
 │  [stage F] absorbs the browsing API + webauth + Angular UI             │
 └──────────────┬──────────────────────────────▲──────────────────────────┘
      odin restic nightly              Mac POLLS (one-way WireGuard intact)
                      ┌────────────────────────┘
 Mac = stateless GPU worker: `runner` (Rust) polling the queue, driving
 three Python model shims — mlx-whisper, pyannote, mlx-lm. Nothing stateful.
```

Principles, each argued in the decision record:

1. **Recorders own their audio until eviction.** Delivery is store-and-forward:
   record → cache → upload → verify the receipt → keep anyway, until local
   cache pressure evicts the oldest *verified* segment. No recorder ever
   deletes because a server said so.
2. **Isis is the system of record and the only always-on service.** One Rust
   daemon, `recalld`, owns the ingest plane, the room stream, and the queue —
   and, by the final stage, the browsing API.
3. **The Mac is a stateless GPU worker.** If it dies, every other recorder
   keeps recording *and delivering*; the loss is bounded to its own microphone
   going forward plus its own unuploaded cache.
4. **The one-way VPN is untouched.** Recorders push to Isis; the Mac polls
   Isis; nothing ever initiates toward the Mac.
5. **The ingest plane is append-only.** There is no delete on any network
   surface; destruction stays an operator act, behind the backup chain.
6. **Python survives only where a model is called.** Three shims and the
   training tools; everything else has a named retirement stage.

## Decision record

The ten questions the proposal had to answer, decided 2026-09-05.

1. **Format — FLAC on the wire; lossless rolling window; Opus tail.**
   Recorders deliver FLAC (mono, native rate). Isis keeps lossless for a
   rolling window (~30 days at current volume — set by measured disk, see
   Storage below) and transcodes the tail to Opus 32k, kept forever.
   *Why:* selection needs no phase, but the spatial/TDOA tier is the one
   unmeasured lever on the worst measured quality problem — attribution near a
   speaker change ([pipeline.md §4](pipeline.md)), evidence one microphone
   cannot carry. Opus at the source would weld that door shut permanently;
   lossless forever is weeks of disk. The window keeps the door open on
   exactly the audio experiments would run on. The upload protocol itself is
   container-agnostic (the filename carries the extension): a recorder flips
   to FLAC when its capture path does, and delivers what it has meanwhile.
2. **The sweep veto's job moves to eviction rules + the backup chain.** See
   "Deletion authority" below. The short form: a receipt triggers nothing; only
   local cache pressure deletes; the ingest plane has no delete endpoint; Isis's
   copy is behind odin's nightly restic and the Mac's off-site copy of it.
3. **"Isis has it" = the recorder re-hashed its own file and the receipt's
   sha-256 matched.** A 2xx is not proof and never triggers deletion — both
   halves of the meeting-recorder litigation
   ([meeting-recorder.md](meeting-recorder.md)) hold. What differs, deliberately:
   continuous capture cannot keep everything on a phone forever, so eviction on
   *cache pressure* replaces "only a person deletes" — but eviction eats only
   segments whose hash was verified, oldest first, and never the open one.
4. **Per-segment timing is in phase 1, by construction.** Closed segments carry
   their capture stamp in their name (`<source>-YYYYMMDDTHHMMSS.<ext>`, UTC,
   the recorder's own clock at segment open). The one-offset connection rebase
   is retired with the streaming protocol, not repaired. Name-vs-arrival is
   *delivery latency* under this protocol (a cached backlog arrives late,
   legitimately), so clock skew is measured separately: the upload carries the
   recorder's send-time, and the server stores it beside its own receive-time.
   A skewed clock is recorded and flagged, never refused — completeness
   outranks precision, same rule as today.
5. **Isis produces the room stream, in Rust.** Selection is envelope alignment
   plus a calibrated per-block rank — no STFT, no model. The Mac fetches one
   stream and transcribes once (#1388). Fusion is not built; if coherent
   combination is ever attempted it starts from the lossless window, which is
   why decision 1 matters.
6. **The USB mic path keeps our code out of capture.** The uploader reads
   *closed* files from disk; sox → ffmpeg stays exactly as deployed. Machines
   flip to FLAC by changing the ffmpeg segment codec, nothing else.
7. **Retention: Opus forever, lossless windowed.** ~1 GB/day Opus is years per
   terabyte; design.md §10's open question is closed by this file.
8. **Live survives, structurally simplified.** The runner takes the *newest*
   room segment first, backfill fills the rest; live and worker become one
   path. The latency floor is segment length + upload + poll (~2 min today) —
   accepted; latency is explicitly not a requirement
   ([design.md §1](design.md)), and #1383's stalls were a property of the path
   this deletes. Segment length stays a recorder parameter if that floor ever
   needs lowering.
9. **The archive and corrections migrate through the same front door.** The
   Mac's audiod backfills its master archive through the ingest plane like any
   other upload (bounded, idempotent, hash-verified). Rows are already on Isis
   — it has been the system of record for them since the split.
10. **A fourth credential plane: per-device, write-only ingest tokens.** See
    "Credential planes". Not the sync token (opens all of `/sync/*`), not the
    device token (creates sessions), not login-free (accepting gigabytes is
    not a pause button). A stolen recorder can append audio and do nothing
    else, and is revoked individually.

## Components

### Recorders

Three implementations, one contract:

| recorder | capture | store-and-forward |
|---|---|---|
| Mac USB mic | `audiod capture` (deployed): sox → ffmpeg segments | `audiod upload` (stage B): watch closed segments, deliver, verify, record state |
| Linux hosts (geb) | `audiod capture` via nix — sox reads ALSA on Linux | same binary, same uploader |
| phones | Kotlin / Swift apps, today streaming PCM | record closed segments via the platform encoder; upload with the same protocol (stage C) |

The recorder contract, in full:

- Record fixed-length segments (60 s today) to local storage, named
  `<source>-YYYYMMDDTHHMMSS.<ext>` from the recorder's own UTC clock at
  segment open. The capture thread never blocks on anything the uploader does.
- Upload each closed segment: `PUT /ingest/v1/segments/{source}/{filename}`
  with its bearer token; compare the receipt's `sha256` against a local
  re-hash. Match → mark verified. Mismatch or error → retry with backoff;
  the file stays.
- Evict only under cache pressure (a configured ceiling), only verified
  segments, oldest first, never the open segment.
- Honour pause: recorders poll the control plane's pause state (the phones
  already do, for their UI); a paused household records nothing anywhere.
  The Mac's local `capture_paused_until` break-glass file keeps working.
- Upload policy is network-aware on phones: deliver on unmetered networks,
  cache on metered ones. Machines deliver always.
- Heartbeats are unchanged (hourly, credential-free, to the control plane).

### recalld — the Isis daemon

One Rust binary (axum + rusqlite), replacing the Python fleet tier stage by
stage. It owns, in build order:

- **Ingest plane** (stage A): the upload endpoint, an append-only blob tree
  `<data>/ingest/<source>/<filename>`, and `<data>/ingest.sqlite` bookkeeping
  (source, filename, capture start parsed from the name, bytes, sha-256,
  received time, skew flag). Durability order: stream to a temp file while
  hashing, fsync, rename into place, fsync the directory, insert the row,
  then answer. Idempotent: re-upload of identical bytes returns the same
  receipt; a name collision with different bytes is 409 — never overwrite.
- **VAD at ingest** (stage D): silero via ONNX on each stored segment —
  speech seconds per segment, feeding liveness ("active" = recent segment
  with speech), the quiet review's evidence, and room prioritisation.
- **Room builder** (stage D): align sources per block (tier-1 envelope
  correlation — works on everything, including the Opus tail), rank by
  calibrated speech level, emit `room-<UTC>.flac` segments into the same
  store plus queue rows. Calibration is maintained per device from what each
  actually records (rolling floor/speech percentiles), which is what makes
  the rank mean "how well is this mic hearing the speaker, for this mic"
  ([audio-plane.md](audio-plane.md)).
- **Work queue** (stage E): jobs out (`transcribe-room` first; refine, ask,
  and the rest absorbed from `/sync/jobs` later), results in (turn rows,
  written with the same SQL the Python store uses — copied, not re-derived,
  the `audiod::store` precedent).
- **Retention** (stage D): transcode blobs past the lossless window to Opus;
  enforce the window.
- **Browsing API + webauth + static frontend** (stage F): the FastAPI surface
  ported route-group by route-group; the Angular app unchanged, its typed
  contract regenerated from Rust types.

recalld and the existing Python `recall api` run side by side in the pod until
stage F retires the latter. recalld owns `ingest.sqlite`; `recall.sqlite`
remains the transcript system of record (shared, WAL, busy-timeout — the same
multi-process discipline the Mac's own agents use on their copy). The
audio-plane / meaning-plane split of [audio-plane.md](audio-plane.md) is thereby
preserved on Isis: blobs + ingest.sqlite are the audio plane; recall.sqlite is
meaning.

### runner + model shims — the Mac worker

`runner` (Rust, stage E) is the whole Mac orchestration: poll recalld for the
next job (newest room segment first), fetch the blob, drive a local model shim,
push the result, ack. It replaces worker, live, jobs, sync-push, outbox and
capture-mirror — a stateless poller needs no watermark, no outbox, no mirror
queue, because the queue lives on Isis.

The shims are the Python floor: long-lived processes speaking JSON over stdio,
one per model family —

| shim | wraps | serves |
|---|---|---|
| `asr` | mlx-whisper | transcription, word timings |
| `voices` | pyannote | diarization, embeddings |
| `llm` | mlx-lm | summaries, ask (stays behind llm-host's one-holder rule) |

A shim holds weights, takes one job at a time, and does no I/O beyond its
stdio and the audio path it is handed. Model choice per job stays a queue
field, so the non-turbo `large-v3` lever ([pipeline.md §2](pipeline.md)) is a
config change once #1388's capacity win lands.

### Credential planes

The three existing planes are untouched
([isis-migration.md](isis-migration.md)); this adds the fourth:

| plane | credential | can |
|---|---|---|
| browsing | Nextcloud SSO session | read/write the UI's API |
| recording control | none (network-gated) | pause state, liveness, heartbeats |
| device upload | `RECALL_DEVICE_TOKEN` | `POST /api/sessions` only |
| **ingest (new)** | per-device token | `PUT` its **own** source's segments; nothing else — not read, not list, not another device's source |

The token table (`RECALLD_INGEST_TOKENS`, or `--tokens <file>` in dev) holds
one `<source> <token>` per line, supplied from the k8s secret — never in the
image, never in the nix store. One widening: a `*` line grants a token every
source, still write-only — the Mac's backfill grant, because its archive
holds every device's master plus a new source per uploaded meeting, and an
enumerated list would drift with each one. Devices never get `*`. Unconfigured = open, the repo's standing inert-unless-configured
pattern, so dev and tests need no ceremony. The read side (listing, blob
fetch, the queue) takes the Mac's sync token. A phone that can upload still
cannot read a transcript — the property that motivated the third plane,
preserved in the fourth.

## Storage, retention, bandwidth

Measured 2026-09-05, method noted so the numbers can be re-derived rather than
trusted: five sources produced 104 MB Opus in 2 h 22 min (`du` over the source
dirs), so continuous capture is ~1 GB/day compressed; lossless mono at native
rates is an order of magnitude more, ~20 GB/day. Isis has 1.1 T free (`df` on
the PVC's filesystem). A ~30-day lossless window is therefore ~600 GB — inside
the budget with headroom, and the knob to turn first if it tightens. The Opus
tail at ~1 GB/day is years per terabyte; retention of the tail is *forever*.

Bandwidth is the one unmeasured prerequisite: lossless delivery sustains
~2 Mbit/s aggregate from the house to Isis. Stage B's acceptance includes
measuring the real sustained rate; if the uplink cannot carry lossless, the
recorders still deliver (the protocol doesn't care), the cache absorbs the
difference, and the fallback is explicit — constrained recorders stay on Opus
and the lossless window narrows to the microphones that matter most for TDOA.
Phones defer upload on metered networks by default.

Isis CPU (4 cores, shared with Nextcloud): VAD, the room builder and the Opus
transcode are each order-of-magnitude ~1 core-hour per day at current volume —
estimates, to be measured in their stages, with the room builder's measured
90x-realtime Mac figure as the anchor ([audio-plane.md](audio-plane.md)).

## Deletion authority — what replaces the sweep veto

The fleet's threat model is destruction, not observation. Today the Mac's
master archive refuses destructive orders from Isis (the sweep veto,
[isis-migration.md](isis-migration.md)). Isis-as-master redistributes that
protection rather than dropping it:

- **No network path deletes.** The ingest plane is append-only; recalld
  exposes no delete. Quiet-review sweeps of speechless capture remain an
  operator-plane act on Isis, now backed by Isis's own VAD evidence — and
  they no longer cascade anywhere, because nothing obeys deletion orders.
- **Recorders never obey.** Eviction is a local decision under local cache
  pressure. Isis's word can cause *nothing* to be destroyed on any recorder;
  a compromised Isis can at worst lie about receipts, which slows eviction
  (the safe direction) or — with a forged matching hash it cannot compute
  without the bytes it claims to hold — is caught by the re-hash.
- **The backup chain holds the tail risk.** odin pulls a nightly restic of
  Isis (SQLite snapshot + blob rsync — the ingest tree lives on the same PVC
  and rides the same job); the Mac keeps its off-site copy of odin's repo.
  The window in which one machine holds the only copy is upload → next
  nightly run, and recorder caches typically span multiple such cycles
  (machines hold days–weeks at their ceilings; phones hours–days).
- **The Mac's master archive is not surrendered early.** Until stage F its
  archive stays complete and protected exactly as today; eviction on the Mac
  is enabled last, after Isis + backups have carried the full load through
  real weeks.

## Pause and liveness under store-and-forward

Pause authority is unchanged: intent lives on Isis, the Mac keeps its
break-glass file, and *recorders stop recording* rather than the server
refusing bytes — a paused household produces nothing to upload. Liveness
inverts cleanly: today the ingest socket's `.alive` marker says "streaming";
under store-and-forward, "active" is a recent delivered segment bearing
speech (recalld's VAD), which is the same promise — a dot the audio can back
— with delivery latency added. Heartbeats continue to cover the
dead-app-while-paused gap they were built for ([devices.md](devices.md)).

## Migration ladder and work packages

Stages land in order; each is shadow-first and per-device where it touches a
live recorder; nothing Python dies before its replacement has survived real
days. Work packages are written to be delegable: each names its context, its
contract, and what proves it. Every package lands green through the full gate
(`nix run ../dev-lint#gate -- . gate.json`) and follows
[conventions.md](conventions.md) — TDD, strict lints, no warnings.

### Stage A — recalld ingest plane (additive; touches nothing live)

*Stage A is live 2026-09-05: A1–A4 built and deployed via A5 (the kubes
model grew a `Sidecar`; the fleet image carries `recalld` and the pod runs
it beside the api).*

- **A1. Crate + skeleton.** New `recalld/` crate (axum, tokio, rusqlite
  bundled, sha2, tracing), mirroring `audiod/`'s lint posture
  (`unsafe_code = "forbid"`, pedantic clippy). Binary `recalld` with
  `--root`, `--bind`, `--tokens`; `GET /ingest/v1/health`. Gate rows: fmt,
  clippy, test (copy audiod's three in `gate.dhall`, regenerate `gate.json`
  via dhall-to-json). *Proof:* gate green; health answers in a test.
- **A2. Blob store + receipts.** `PUT /ingest/v1/segments/{source}/{filename}`
  with the durability order, naming validation (source dir = name prefix,
  stamp parses, extension allowlisted: flac/opus/ogg/wav), idempotency, 409
  on divergent re-upload, size cap, skew flag. `ingest.sqlite` schema +
  row insert. *Proof:* tests for round-trip hash, idempotent re-PUT,
  divergent 409, bad names, a truncated body never producing a row or a blob.
- **A3. Token plane.** Tokens file, per-source authorization, inert when
  unconfigured, constant-time compare. *Proof:* tests for wrong token, right
  token/wrong source, unconfigured-open.
- **A4. Read side.** `GET /ingest/v1/segments?source=&since=` (rows) and
  `GET /ingest/v1/blob/{source}/{filename}`, gated by the sync token.
  *Proof:* list/fetch tests incl. auth.
- **A5. Deploy.** The Dockerfile's Rust stage (done with A1) puts `recalld`
  in the one fleet image; the pod runs it as a second container from the
  same image. The monorepo's kubes model (`dhall/lib/types.dhall`,
  `render.dhall`) models one container per Workload plus DB sidecars, so
  this needs a modelled second-container field, not a hand-edit: same
  image, own command (`recalld --root /data --bind 0.0.0.0:8001 --tokens
  /secrets/ingest-tokens`), the same PVC mount (RWO — same pod is what
  makes sharing it legal), a tokens file projected from `recall-secret`,
  `RECALLD_READ_TOKEN` env, and a second wg-bound hostPort (8001) beside
  8000. Also: the PVC's modelled 50 Gi is sized for today's mirror, not
  the stage-D lossless window — revisit `storageGi` when D5 lands, not
  now. Verify odin's backup job covers the ingest tree (it rsyncs the
  whole PVC — confirm, don't assume). Host-touching; deploy with
  `kubes/deploy.sh recall` per the monorepo's docs.

### Stage B — the Mac delivers (audiod upload)

*Live 2026-09-05: A5 deployed (the pod runs recalld beside the api, wg
hostPort 8001, write gate proven up by a refused wrong-token PUT), B1's
agent wired, and the first deliveries verified end to end — a blob fetched
back from Isis hashes identical to the Mac's master. B2's first measurement:
200 segments in 50.1 s, zero failures, wall time all network wait — ~4
deliveries/s sequential, ~4.2 Mbit/s effective at the archive's smallest
segments. That clears continuous capture (~5 segments/min) by ~50x and the
~2 Mbit/s lossless floor with room; re-measure at FLAC segment sizes when
B3 lands.*

- **B1. Uploader.** `audiod upload --root <archive> --url <base>`: scan for
  closed segments, deliver oldest-first, verify receipts, record state in an
  audiod-owned `upload-state.sqlite` under the archive root. Never touches
  the open segment; wholly off the capture thread (separate process).
  Launchd timer agent in `deploy/hm-agents.nix`. *Proof:* tests against a
  stub server — receipt match, mismatch retry, crash-resume idempotence.
- **B2. Measure.** Sustained upload throughput and archive backfill rate on
  the real link (decision-record bandwidth gate). Record findings here.
- **B3. FLAC on machines.** Flip `audiod capture`'s ffmpeg segment codec to
  FLAC behind a flag; shadow first (`docs/audio-plane.md` cutover rule).
- **B4. The doctor learns delivery.** *Done 2026-09-05:* `delivery_checks`
  grades the backlog by its oldest member's age (both sides counted — the
  disk scan against the state db, so completeness is the same check) and
  WARNs on any journaled 409, naming the files. Quiet where the uploader
  has never run. The whole archive backfilled the same day: every
  grammar-matching segment delivered and verified, zero conflicts.

### Stage C — phones and geb flip, streaming retires

- **C1. Android store-and-forward.** *Shadow built 2026-09-05:* the mic loop
  tees into capture-stamped closed segments (`SegmentWriter`/`SegmentStore`,
  the meeting queue's state-is-a-directory idiom), delivered by
  `SegmentUpload` with the receipt re-hash rule, unmetered-only, evicting
  verified-delivered oldest-first under a ~2 GiB ceiling and never anything
  else. WAV first, deliberately: the protocol is container-agnostic and
  MediaCodec's FLAC header behaviour gets probed on-device (C1b) rather
  than assumed. Streaming is untouched; a segment never spans a reconnect
  gap (the name claims continuity from its stamp).
  **Open, and Pippijn's: the mic-open gate at the C4 flip.** Today the mic
  opens only while the stream connects to the Mac — a home-presence proxy.
  Store-and-forward decouples recording from delivery, so the gate must be
  chosen: keep Mac-connect (outage still silences phones), gate on home
  presence (recommended — survives a Mac outage, keeps recording inside
  the consent boundary), or record whenever unpaused (records outside the
  house — a widening only he can choose). Shadow behaves identically under
  all three.
- **C2. iOS store-and-forward.** Same, AVAudioFile/kAudioFormatFLAC.
- **C3. geb.** Replace `python -m recall.mic` with `audiod capture` +
  `audiod upload` under systemd via nix.
- **C4. Retire streaming.** After every device has flipped and survived real
  days: delete the TCP ingest path (`audiod::server`, `rebase`,
  `recall.mic`, `beat_relay` LAN fallback if subsumed), and the `.alive`
  marker with it. Per-device, one at a time, confirm each records+delivers
  before the next ([devices.md](devices.md) update rule).

### Stage D — the room stream on Isis

- **D1. Shared DSP crate.** *Done 2026-09-05:* one workspace
  (audiocore + audiod + recalld, one lockfile), `audiocore` holding the DSP
  (`align`/`envelope`/`decode`/`stft`/`fuse`/`wav`), the offline instruments
  (`align_probe`, `fuse_window`) and — deliberately — the ONE segment-name
  grammar (`names`, recalld's typed parser merged with the sweeps'
  stamp/glob readers). It also bought the test the stub deferred: audiod's
  uploader now proves delivery, the auth gate and the 409 path against the
  REAL recalld router (`audiod/tests/upload_real_server.rs`).
- **D2. Calibration.** *Measuring since 2026-09-05:* recalld's background
  scanner decodes every delivered segment once (ffmpeg, bounded batches)
  and stores its speech/floor quantile levels (`segment_levels`); the
  per-device reference is a QUERY over a source's own recent rows
  (`levels::speech_reference_db`) — calibrate.py's faintest-speech
  measurement re-derived continuously from delivery instead of once by
  hand. D3's rank consumes it; uncalibrated rank degenerates to the fixed
  choice ([audio-plane.md](audio-plane.md)).
- **D3. Room builder.** *Built 2026-09-05, running in shadow:* one settled
  UTC minute at a time (15 min settling for delivery latency), calibrated
  per-block selection — each source's block level against its OWN D2
  reference, a newtype (`CalibratedDb`) so raw loudness cannot cross the
  rank boundary — carrying the winner's audio whole into
  `room-<stamp>.flac` (16 kHz mono, ASR's shape) with full provenance per
  block. No verdict on partial evidence: unmeasured overlap defers. **The
  rank is RAW speech level for now — calibrated selection is parked**: the
  first build under the calibrated rank handed phones 30% of all blocks and
  the referee failed it (room 0.321 vs usb 0.229); with the real-speech
  reference gate (levels::REAL_SPEECH_MARGIN_DB) usb rose to 92% overall
  but pixel9 still took 13/29 of the June referee window where usb is
  best throughout — so per the pre-stated rule the rank formula is
  indicted, raw level (the bake-off's tying arm) chooses, and the
  calibrated rank rides along in provenance until D4's VAD gives the
  reference an honest speech gate. Because the builder runs over the
  delivered archive, the referee (room vs best-single, June corpus) runs
  OFFLINE and remains the acceptance gate before stage E lets anything
  transcribe room.
- **D4. VAD at ingest** (silero ONNX). Liveness + quiet evidence + priority.
- **D5. Retention.** Window transcode to Opus + enforcement, measured cost.

### Stage E — the queue and the runner

- **E1. Queue in recalld.** *Built 2026-09-05, lean:* jobs are DERIVED from
  room segments (the share-upload lesson — a missed enqueue cannot strand
  audio), leased newest-first with a 10-minute TTL (`PUT /work/v1/lease`,
  `PUT /work/v1/jobs/{id}/done`, the sync-token plane), results stored
  opaque until E3 interprets them into turn rows. Long-poll and the resolved
  result-writing are E3's.
- **E2. Shim protocol + `asr` shim.** JSON-over-stdio contract; the
  mlx-whisper shim carved out of `recall.asr` with vocabulary biasing kept.
- **E3. runner.** The Rust poller: newest-first, fetch, shim, push, ack;
  launchd agent. Runs beside the old worker in shadow on the same audio
  until outputs agree, then the flip: worker/live retire, per-source
  transcription becomes backfill jobs.
- **E4. Absorb the rest of `/sync/jobs`.** refine (via the `voices` shim),
  ask (via `llm`), ab-compare; retire `recall.jobs`, `sync_push`, outbox,
  capture-mirror (pause intent moves to a recalld long-poll the runner
  mirrors — same edge-trigger semantics).

### Stage F — recalld absorbs the browsing tier; the Mac lets go

- **F1. Port the API route-group by route-group** (reads, labels, capture,
  devices, quiet, recall/ask, sessions), webauth (Nextcloud OAuth +
  HMAC-signed cookie), static frontend serving; regenerate the Angular
  contract from the Rust types; retire `recall api` and the Python fleet
  image tier.
- **F2. The Mac joins the recorder contract fully.** Eviction enabled at a
  generous ceiling; the "master archive" title passes to Isis + the backup
  chain, deliberately and last.

## What stays Python, and what dies when

The floor, permanent: the three model shims (mlx-whisper, pyannote, mlx-lm)
and the training/evaluation toolchain (finetune, pilot, export, wer, golden
checks) — Python because the models are Python, per
[design.md §9](design.md).

Everything else in `src/recall/` retires with its stage: the mic/streaming
client and relay with C4; worker, live, sync-push, outbox, jobs and
capture-mirror with E3–E4; the API modules, store, webauth and schemas with
F1. The authoritative list is `ls src/recall` against this ladder, not a
table copied here; when a stage lands, its deletions land in the same change.
