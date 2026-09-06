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
 │  room builder: tier-1 align + raw selection [1]          exists)       │
 │  work queue → jobs out, results in                                     │
 │  [stage F] absorbs the browsing API + webauth + Angular UI             │
 └──────────────┬──────────────────────────────▲──────────────────────────┘
      odin restic nightly              Mac POLLS (one-way WireGuard intact)
                      ┌────────────────────────┘
 Mac = stateless GPU worker: `runner` (Rust) polling the queue, driving
 three Python model shims — mlx-whisper, pyannote, mlx-lm. Nothing stateful.
```

[1] Calibrated selection is built and PARKED — see D3 below. The rank is
    recorded in provenance; raw level chooses.

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
2. **The sweep veto's job moved to eviction rules + the backup chain** (done
   2026-09-06: the deletion-order channel is deleted, not merely vetoed). See
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

## What must survive — and what is therefore disposable

**DECIDED 2026-09-06 by Pippijn: only the RECORDING has to survive. All
processing may be changed at will; the product is being REBUILT, not
transported, and the result does not have to be identical to today's.**

That single sentence changes the shape of every stage below it, so read it
before the ladder. It replaces "port the Python faithfully" with "keep the
audio, rebuild the rest", and the difference is most of the remaining work.

**Not re-derivable — these are the system of record:**

| what | rows (2026-09-06) | why it cannot be recomputed |
|---|---|---|
| the audio itself | 11 920 segments | requirement #1; a gap is the worst failure |
| human corrections | 468 | a person listened and typed; the fine-tune corpus and enrolment seed |
| enrolled speakers + voiceprints | 9 / 958 | seeded from corrections and confirmed turns |
| vocabulary terms | 5 | hand-managed proper nouns |

⚠ Corrections are NOT "the recording", and they are kept anyway. They are the
other human input in the system, they cost real time, and #1461 is blocked on
making more of them. Treat the pair — audio plus what a person said about it —
as the thing that survives.

**Everything else is a derived view and may be dropped or recomputed:**
82 235 transcript rows, of which **52 423 are hidden and 11 163 superseded —
64% is invisible weight** carried by every query, every migration and every
port; plus 45 639 embeddings, 19 tables and 43 migrations of accreted schema.

Consequences, and they are large:

- **The browsing tier is REBUILT, not ported.** The 8 968 lines of `api_*`,
  `store`, `store_schema`, `schemas` and `webauth` do not need a faithful
  translation; recalld gets a clean schema of a handful of tables and the
  current view, and the history stays behind in the old database.
- **Byte-parity with the Python stops being a goal.** A parity gate would fail
  on the first deliberate improvement. The read port was verified against the
  real archive once (see F1) and the harness was then retired on purpose.
- **A cut feature needs no port at all.** The fastest route to less Python is
  deleting surfaces the product no longer has, not translating them.

### Scope of the rebuilt product

**DECIDED 2026-09-06 by Pippijn.** KEPT: the core memory aid — timeline,
search, playback, correction, speaker attribution — plus **meetings/sessions
upload** (the Android recorder and its device-token plane).

CUT: **Ask** (LLM Q&A over the archive), **day summaries**, **Compare / A-B**,
and the **quiet-review** operator surface.

⚠ **Cutting Ask MUST NOT take `llm-host` with it.** The one-holder daemon
(`recall.llmhost`, 127.0.0.1:8092) is also the model holder for a DIFFERENT
project — `life/tools/emotion_worker.py` addresses it directly over loopback.
Deleting it would break life silently, from a change made in this repo for
unrelated reasons. The daemon and its launchd agent stay; what goes is recall's
own consumption of it.

⚠ Cutting the quiet review does not mean junk returns to the read path. Under a
rebuilt schema the sweeps become a filter at derivation time rather than a
`hidden_reason` column plus a review UI — which is also why 52 423 hidden rows
need not travel.

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

The fleet's threat model is destruction, not observation. The Mac's master
archive used to refuse destructive orders from Isis (the sweep veto,
[isis-migration.md](isis-migration.md)); since 2026-09-06 it receives none,
because the channel was removed rather than guarded — the veto, its refusal
journal and its doctor check went with it. Isis-as-master redistributes that
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
  *Verified end to end 2026-09-05: pixel5's shadow WAVs delivered to Isis
  under its own token during a live test; per-device tokens live for all
  four phones.*
  **DECIDED 2026-09-06 by Pippijn: RECORD WHENEVER UNPAUSED.** The mic opens
  whenever capture is not paused, regardless of the Mac or of being at home.
  The alternatives were keeping Mac-connect (an outage silences every phone,
  the failure store-and-forward exists to end) and gating on home presence.
  ⚠ **This deliberately widens capture BEYOND the house** — cafés, other
  people's homes, other people's conversations — and that is a consent
  decision, which is why it was his to make and not a default to infer. The
  pause remains the whole control surface, so it becomes the thing that must
  always work: everything else can degrade, that cannot.
- **C2. iOS store-and-forward.** *Built and installed 2026-09-05:* the
  Swift mirror of C1 (SegmentStore/Writer/Upload, WAV first, receipts
  re-hashed, evict-under-pressure), tee gated on the CONNECTION — on iOS
  the mic stays hot even while paused, so the connection is the one signal
  meaning at-home + unpaused. Token provisioned via the app's data
  container over devicectl.
- **C3. geb.** *Cut over 2026-09-05* — the LAST Python recorder retired:
  `audiod capture` (ALSA producer via ffmpeg, geb's own proven device
  path) + `audiod upload` + `audiod pause-mirror` under systemd
  (nixos-config `machines/geb/recall-recorder.nix`; audiod pinned by
  out-link, see the module's bump note). First store-and-forward delivery
  verified on Isis within a minute of capture. Transitional and accepted:
  geb no longer beats or streams, so the old liveness reads it stale until
  the delivery-based liveness lands (see D4/liveness below).
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
- **D3. Room builder.** *Built 2026-09-05, running in shadow:* one settled UTC
  minute at a time (15 min settling for delivery latency), the winner's audio
  carried whole into `room-<stamp>.flac` (16 kHz mono, ASR's shape) with full
  provenance per block. No verdict on partial evidence: unmeasured overlap
  defers. `CalibratedDb` is a newtype so a raw level cannot cross the rank
  boundary by accident. **Raw speech level chooses; the calibrated rank is
  recorded in provenance and parked** — see the acceptance note below for why,
  which is now a statement about the CORPUS rather than about the rank.

  Because the builder runs over the delivered archive, the referee (room vs
  best-single) runs OFFLINE and is the acceptance gate before stage E transcribes
  room.

- **D3 NOT ACCEPTED — calibrated selection RE-PARKED 2026-09-06, and this time
  the reason is the corpus, not the rank.** The reference is now VAD-gated
  (stage D4's detector rather than a loudness proxy), which is a real
  improvement and is kept. What is NOT kept is letting it choose.

  The June window passed: cleared and rebuilt by the real builder, 29/29
  `built:calibrated`, zero deferrals, usb winning all 29, median WER 0.229 both
  arms. But that window compares IDENTICAL AUDIO — usb wins there under both
  ranks — so it was never evidence, exactly as it had been flagged.

  ⚠ **Where the ranks DO differ, the corpus cannot test them at all.** Census
  over the whole archive: they disagree on **1290 of 2664 rankable blocks
  (48%)**, systematically moving blocks off the condenser onto phones (usb ->
  iphone11 448, usb -> pixel5 281, usb -> geb 242, usb -> pixel9 238). Ground
  truth is mid-June — 328 of 468 corrections fall on 14-16 June — while the
  disagreements are September (1127 of 1290). **They overlap on 8 minutes:
  1.7%.** That is structural, not sampling: the corrections predate the
  multi-device fleet, so there were barely two microphones to disagree about
  when they were made.

  Raw has MEASURED parity with best-single (median 0.229, twice). Calibration
  has no measurement anywhere it differs. Shipping it would be a verdict on
  partial evidence — the thing the builder already refuses for a single block —
  applied to half of them. Nothing consumes room yet, so parking costs nothing.

  **TO DECIDE IT:** ground truth on SEPTEMBER minutes where the ranks differ,
  then the referee on that window. The census names the densest hours
  (2026-09-02T19, 2026-09-03T19, 2026-09-04T20). This is a DATA task, not a code
  one, and it is what #1388's quality half now waits on.

  ⚠⚠ **READ THE MEDIAN, NOT THE MEAN, and the harness prints the mean.** This
  run's mean was usb 16.449 / room 12.083; the previous night's, on the SAME usb
  audio, was 0.666 for both. The control moved 25x while the median did not move
  at all. The cause is ASR hallucination loops (#1410): on a one-word utterance
  the model emits "As to As to As to…" hundreds of times, scoring WER 223. Four
  of 38 cases; excluding them the means are usb 0.347 / room 0.343. The loops
  are not stable run to run — identical CONTENT through a different encode path
  flips them — so no decision may rest on a mean over this corpus.

- **D4. VAD at ingest** (silero ONNX). Liveness + quiet evidence + priority.
  *Detector built 2026-09-05:* `recalld::vad` runs silero through `ort`, the
  network EMBEDDED in the binary (`include_bytes!`) so no rollout can forget a
  model path. Verified on real speech rather than tones — a sine proves nothing
  about a speech model. ⚠ Three environment gaps that macOS hid, all found by
  building on amun rather than trusting the laptop: the Linux link needs `g++`
  (onnxruntime is C++), `ort`'s default `tls-native` drags in openssl that
  `rust:1-slim` lacks (rustls instead), and silero v5+ prepends 64 samples of
  CONTEXT — omitting it is accepted silently by the dynamic input shape and
  returns near-zero probability on obvious speech, which reads as a quiet room
  rather than a bug. A golden probability trace pins that contract everywhere,
  because the real-speech fixtures are gitignored (public repo, see #1433).
  *Scanner built 2026-09-05:* `recalld::speech` measures every delivered
  segment in bounded batches, oldest first, one row per blob for ever, with an
  UNKNOWN sentinel (-1 s) so "we could not look" can never be read as "nobody
  spoke" by a sweep. Inference is pinned to one thread — a background
  measurement must not saturate a 4-core box shared with Nextcloud.

  ⚠ **The runtime is DLOPENED, not bundled, and that is load-bearing.** ort's
  prebuilt ONNX Runtime requires AVX2; isis (Xeon E3-1225 V2) and amun
  (E3-1245 V2) are Ivy Bridge, 2012, and AVX2 arrived with Haswell in 2013.
  Calling it there did not degrade — it raised SIGILL and killed the daemon that
  IS the system of record (measured 2026-09-05: recalld crash-looped, exit 132,
  five restarts, ingest refusing connections until the image was pinned back).
  The fix is Debian's `libonnxruntime`, built for baseline x86-64: `ort` uses
  `load-dynamic` at `api-21`, the image installs `libonnxruntime1.21`, and
  `ORT_DYLIB_PATH` names it by VERSIONED soname so an apt upgrade cannot swap the
  ABI under a running image. The devshell and the nix check derivation supply the
  same variable, so local, sandbox and production share one mechanism.

  ⚠ **The session is a process-lifetime singleton that is NEVER DROPPED.** With
  dynamic loading, ONNX Runtime's destructors run after the library is unloaded:
  on amun every test PASSED and the binary then died with SIGSEGV on exit. A
  daemon that segfaults on shutdown is not shippable, and "all assertions green"
  is not the same as "the process survived".

  ⚠ **Verified by RUNNING on the target, not by building for it.** The first
  attempt built the image on amun, called Linux proven, and shipped a daemon that
  crash-looped on isis — amun shares isis's CPU generation, so executing the
  suite there would have caught it in seconds. It now runs on amun under the
  exact Debian runtime production uses: 13 tests green, exit 0, and the golden
  probability trace IDENTICAL to macOS under a different ORT version and
  architecture, which is what makes that trace worth keeping.

  *Wired into liveness 2026-09-05:* `/ingest/v1/liveness` is SPEECH-GATED, which
  keeps the promise the `.alive` marker already made — a dot the audio can back,
  so a room of digital silence reads idle on purpose. ⚠ Only a segment MEASURED
  AS SILENT disqualifies: unmeasured and undecodable ones still count, because
  the scanner runs BEHIND live audio and "not looked at yet" is not evidence of
  silence — treating it as silence would black out every recorder the moment it
  ships. The scan therefore runs NEWEST FIRST (both consumers read recent rows),
  with the archive backfilling behind, the same priority the work queue takes.

  *Measured in production:* ~98 segments/min at ~0.9 core (pod 189m -> ~1080m,
  load 1.22 -> ~2.5 on 4 cores), the 15.8k backlog clearing in ~2.6 h. The gate
  does real work rather than passing everything through — usb's newest DELIVERED
  segment was 21:12:29 while its newest SPEECH was 21:02:29, ten minutes of
  measured silence correctly excluded. Per-device ratios differ the way
  calibration needs: geb 20 speech / 6 silent, pixel5 11/39, oneplus6t 26/24.
  Oldest-first, before the flip, had spent 25 minutes still inside 13-15 June.

  STILL TO BUILD: the rest of the wiring — speech into liveness, the
  quiet review's evidence, room priority, and the calibrated reference that
  un-parks D3's rank.
- **D5. Retention.** Window transcode to Opus + enforcement, measured cost.

### Stage E — the queue and the runner

- **E1. Queue in recalld.** *Built 2026-09-05, lean:* jobs are DERIVED from
  room segments (the share-upload lesson — a missed enqueue cannot strand
  audio), leased newest-first with a 10-minute TTL (`PUT /work/v1/lease`,
  `PUT /work/v1/jobs/{id}/done`, the sync-token plane), results stored
  opaque until E3 interprets them into turn rows. Long-poll and the resolved
  result-writing are E3's.
- **E2. Shim protocol + `asr` shim.** *Built 2026-09-06:* `recall.shim` is the
  contract (line-delimited JSON, one job at a time, `hello` answered by the
  protocol itself so it works even for a shim whose model failed to load), and
  `recall.shim_asr` wraps mlx-whisper. Errors are RESPONSES: a shim that dies on
  one bad clip loses weights that cost seconds to load and strands the queue.
  ⚠ **stdout is the protocol, so nothing else may touch it** — mlx-whisper's
  dependency prints a huggingface progress bar, and one stray line desyncs the
  stream SILENTLY. `serve` keeps a private handle on the real stdout and points
  `sys.stdout` at stderr; a subprocess test pins it.
  ⚠ **The shim reads no database.** `initial_prompt` (vocabulary biasing) is
  CARRIED by the caller — fetching it would put a DB handle and a failure mode
  inside the process whose only job is to run a model, against principle 3.
- **E3. runner.** *Built 2026-09-06, shadow:* the `runner` crate — lease, fetch
  the blob, drive the shim over stdio, push, ack. Stateless by construction: no
  watermark, no outbox, no mirror queue, so killing it costs an expiring lease.
  A shim REFUSAL is terminal and recorded (the clip is the problem); a TRANSPORT
  failure says nothing and lets the lease expire (the shim is). Tested against
  the real recalld router with only the model substituted.

  ⚠ **Running it against the live fleet is what found the silence problem.**
  Transcribing a silent minute does not return nothing — it returned
  "Thank you." twice, and another minute came back as 156 segments carrying a
  150-character run of tildes at 0.19 confidence (#1410). The queue had derived
  a job for EVERY room segment: 1784 of 4288 were measured silent. Derivation is
  now gated on D4's speech evidence, same rule as liveness — only MEASURED
  silence disqualifies.

  STILL OPEN before the flip:
  - **#1461**, which decides what the room stream should be at all.
  - **The vocabulary prompt.** The runner sends none, so its transcripts spell
    household names worse than the old worker's. The shim cannot fetch it by
    design, so it must be carried — by the runner reading it once, or by the job.
    Settle it before ~2500 real jobs are transcribed without it.
  - launchd agent: not yet written; the runner has been run by hand.
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

  *Reads ported 2026-09-06, NOT MOUNTED:* `recalld::reads` serves the
  `/api/search` and `/api/timeline` shapes from `recall.sqlite`, opened
  READ-ONLY — recalld does not own the meaning plane and must not be able to
  write it. Reads went first because a route group that only answers questions
  cannot destroy anything if it is wrong, and because two implementations of one
  contract can be DIFFED.

  ⚠ **It is deliberately not on the router yet, and that is a security
  decision, not an omission.** The browsing plane's promise is a Nextcloud
  sign-in plus a user allowlist; recalld's read side takes the sync token.
  Mounting transcripts on recalld before webauth is ported would open a SECOND,
  WEAKER door to the household's audio. The port lands behind webauth or not at
  all.

  *webauth ported 2026-09-06:* `recalld::webauth` is the SSO gate — the three
  planes (browsing gated, recording login-free, device-token), the stateless
  HMAC-signed cookie, the short-TTL OAuth state, the username allowlist, and
  inert-unless-configured. 11 tests, each named for the attack it stands against.

  ⚠ **The token format is deliberately IDENTICAL to the Python's, and that is
  what makes an incremental cutover possible.** Sharing `RECALL_SESSION_SECRET`
  and the exact `<payload>.<mac>` shape means a cookie minted by the Python OAuth
  flow verifies in Rust and vice versa, so recalld can be mounted behind the
  EXISTING sign-in, route-group by route-group, with no second login and no flag
  day. This is the one place in the rebuild where compatibility is worth keeping,
  and it is kept for that reason rather than for fidelity's sake. A golden token
  minted by `recall.webauth` itself is pinned in the tests: if it ever fails to
  verify, the two halves have stopped recognising each other and incremental
  cutover is off the table — a much bigger fact than a red test.

  Two things the port improved rather than copied:
  - **Expiry is enforced inside `verify`**, behind a trait every claim type
    implements, so a caller cannot be able to forget it. In the Python it is
    checked in `_verify` too, but nothing stops a new reader of the payload
    skipping it; here the type system does.
  - **The device-token compare is constant-time** (HMAC of both sides), where the
    Python's is a plain equality on a secret.

  *The flow and the gate landed 2026-09-06 too:* `/login`, `/auth/callback`,
  `/logout`, `/api/me`, and the middleware — 20 tests, the OAuth exchange driven
  against a REAL stub Nextcloud rather than a mocked client (the likeliest error
  is the request SHAPE, and a mock would have tested my expectation of it), and
  the gate driven through a real router. Mutation-checked: opening the gate fails
  three tests.

  Two properties worth naming, both tested:
  - **The callback rejects a bad state BEFORE any network call**, so a stranger
    cannot make this server dial Nextcloud on demand. The test points the config
    at a dead port, so reaching the network would 502 instead of 403.
  - **A user outside the allowlist gets 403, not 401.** They ARE signed in, and
    401 would loop them through Nextcloud for ever.

  *Mounted 2026-09-06:* `app::router` assembles the browsing plane behind the gate
  and `/api/timeline` + `/api/search` are served from it.

  ⚠ **Here `None` means ABSENT, not open — the one place this repo's
  inert-unless-configured rule is deliberately INVERTED.** Everywhere else an
  unconfigured credential means "run open", which is right for a LAN-only dev box
  and wrong for routes that serve household transcripts: an unconfigured recalld
  answers them with 404 rather than answering them to anyone. A test pins it.

  ⚠ **A cookie is scoped to a HOST, not a port**, which is what makes the cutover
  work in practice rather than only in principle. recalld answers on
  `10.100.0.2:8001` while the Python answers on `:8000`, and a browser sends the
  same `recall_session` to both. With the token format identical, a person signed
  in through the Python is already signed in here — so a route group can move
  between the two with nobody signing in again, and the dash redirect-URI question
  only arises when recalld starts serving the sign-in ITSELF.

  One deliberate divergence, the first: **the read routes clamp `limit`** where
  the Python passes it straight to SQLite. `?limit=10000000` asks for the whole
  archive in one page, and a browsing route a signed-in person can accidentally
  turn into an archive dump will eventually be turned into one.

  *Static serving PORTED but NOT mounted, 2026-09-06:* `recalld::spa` implements
  the three rules a generic static handler would get wrong, all tested, two of
  them bought by incidents rather than designed:
  - an `/api/*` miss is a **404, never the shell** — returning HTML with status
    200 turns "no such route" into a JSON parse failure far from its cause;
  - **`index.html` is `no-cache`, hashed bundles are immutable** — the shell names
    the current bundles, so caching it means a deploy is invisible until a hard
    refresh, which is the bug that served stale code from isis;
  - **a request cannot escape the frontend root** — containment is checked on the
    CANONICALISED path, so `..` and symlinks resolve first. Above `dist/` sit the
    archive, the database and the token file.
  Both the traversal guard and the cache rule are mutation-checked.

  ⚠ **Mounting it is what dev-lint stopped**, and the rule was right. Wiring the
  SPA made recalld a serving ROOT, and `DL-WIRE-ROUTE-DRIFT` resolved its axum
  table against the frontend's call sites: **26 calls that would miss**, because
  recalld serves 2 of the ~28 `/api/*` routes the app makes. Serving the UI from
  here today hands someone a half-working app. Same rule as the read routes
  waiting for webauth — do not expose a surface that is not ready. `app::router`
  gains its `frontend` field when the route groups are done, not before.

  ⚠ **STILL TO DO:** the remaining route groups (labels, capture, devices,
  sessions, audio), then shadow days, then the Python goes.

  *Checked against the running pod, so the next session does not have to guess:*
  `NC_INTERNAL_URL` is `http://nextcloud-server.nextcloud.svc.cluster.local` —
  server-to-server OAuth calls go over PLAIN HTTP in-cluster, presenting the
  public host as `Host:` so Nextcloud's trusted-domain routing treats them like
  the public request. Only the browser-facing authorize URL is https, and that is
  a string the browser follows rather than a call recalld makes. So the flow needs
  no TLS on the deployed path — but the fallback when `NC_INTERNAL_URL` is unset
  IS https, and the workspace's `ureq` is deliberately `default-features = false`
  (no TLS; everything else here speaks plain HTTP inside WireGuard). Enable rustls
  explicitly rather than inheriting that, and ⚠ NOT `tls-native`: ort's default
  dragged in an openssl `rust:1-slim` does not carry, which cost an image build
  once already. The pure half — every decision about
  who may enter — is done and tested; what remains is the HTTP plumbing around
  it. ⚠ And a deployment note that is easy to miss: the redirect URI registered
  on dash names port 8000. Serving the browsing plane from recalld's 8001 needs
  that client re-registered, or recalld taking over 8000 at the cutover.

  *Verified once, against the real archive, then the instrument was dropped:* a
  differential harness asked both implementations the same questions about a
  SNAPSHOT of the 554 MB archive and diffed the JSON — 10 cases over ~30k visible
  turns, byte identical. It is not kept, because as of 2026-09-06 the product is
  being REBUILT rather than transported (see "What must survive" above) and a
  byte-parity gate would fail on the first deliberate improvement. Two findings
  from building it are worth more than the harness was:
  - ⚠ **A live archive cannot be diffed.** Pointed at the real file it reported
    a difference that was not one: Python read a turn with no speaker, the
    identify pass wrote a guess onto it, and the second reader saw the guess.
    Four daemons write that database. Any A/B over it must snapshot first — the
    same rule the v43 migration test follows.
  - ⚠ **A case set is only as good as the ROWS IT REACHES.** The first eight
    cases passed a mutation they should have failed — a confirmed speaker
    keeping its score, a real contract break — because human labels live in July
    and August while those cases sampled the newest pages. 555 visible turns
    carry both a label and a score and not one was being looked at.

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
