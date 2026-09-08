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
6. **Python survives only where a model is called.** Three shims, plus `wer`
   and the golden ASR check; everything else has a named retirement stage.

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
| human corrections | 468 | a person listened and typed; the enrolment seed, and the only human input besides the audio |
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

### What recall is FOR — two use cases, and nothing else

**DECIDED 2026-09-06 by Pippijn.** The product serves exactly two situations.
Anything that serves neither is not a feature, it is weight.

1. **The home room, recorded by several microphones at once.** Continuous
   household capture, multiple mics hearing the same speech, turned into a
   searchable attributed record. This is what the room-stream question (#1388,
   #1461) is *about*: several recordings of one room have to become one
   transcript.
2. **A single recording of a meeting with doctors, in hospital.** One file from
   the phone, uploaded, transcribed and diarized, read back as a clean
   attributed transcript — who said what in an appointment. Not continuous, not
   multi-mic, and the accuracy that matters is proper nouns and medical terms.

The two share a spine (capture -> ASR -> diarize -> attribute -> read) and differ
in almost everything else, which is why naming them separates what must be built
from what merely exists.

#### Use case 2 has no ladder, and its backlog is human, not mechanical

Stages A–F below are entirely about use case 1. Use case 2 was measured against the
archive on 2026-09-06:

| | measured |
|---|---|
| recordings uploaded | 20, roughly weekly, over four months |
| transcribed | 20 of 20 |
| **diarized** | **20 of 20 — every one of the 2 222 visible turns carries a cluster** |
| voices named by a person | 9 sessions; **11 have never been opened and named** |
| rows kept | 10 593 written → 2 222 visible (the rest are the pre-alignment pass) |
| corrections made on them | 24 of 468 |

⚠ **`speaker_label` is the HUMAN name, not the machine's answer.** Diarization writes
`speaker_cluster` (`SPEAKER_00`…); `speaker_label` is filled by the session screen's
naming strip, where a person names each voice once and the label applies to every turn
of that voice. Reading a null `speaker_label` as "diarization did not run" inverts the
finding completely — it says a person has not been here yet, and the machine half is
done. Both `session_summaries` and `name_voice` document this; the query does not.

So use case 2 has no pipeline defect on the evidence available. Its measured gap is
**11 meetings awaiting a few minutes each of naming**, which is the same shape as the
corrections finding below: the machine work is done and the human work stopped. Before
building anything here, that is the fact to act on, and it needs no code.

What use case 2 does **not** need, and this is worth recording because it looks like it
should: a role picker. Naming each diarization voice once per session, applied to all
its turns, with a voiceprint suggestion beside it, is already built and is what the
session screen is.

⚠ **What is NOT established** is quality: whether those clusters split the doctor from
the patient correctly, and whether medical terms and proper nouns survive ASR. Nothing
measures either — `docs/meetings.md` still says a publishable transcript is
hand-cleaned. That is where a real use-case-2 work package would start, and it needs
ground truth on a meeting before it can start at all.

### Training is not a goal

**DECIDED 2026-09-06 by Pippijn: "We don't need to train. We only need to
correct. What we train from that, we can decide later."**

So the LoRA toolchain is deleted (`finetune`, `training`, `hf_asr`, `evaluate`,
`finetune_pilot` — 925 lines), and with it the export/pilot/fine-tune commands
and the adapter branch in the transcriber.

⚠ **ENROLMENT IS NOT TRAINING, and it stays.** Labelling a voice attaches a name
to a voiceprint; that is how attribution works, it is requirement #3, and it is
what separates the doctor from the patient in use case 2. `identify` and `embed`
were checked and are independent of the deleted cluster. Corrections keep
feeding voiceprints — what went is the LoRA machinery, which `design.md` already
recorded as un-deployed since 2026-07-11.

⚠ **Corrections are still collected, and are still not re-derivable.** They are
the human half of the system of record ("What must survive" above). What changed
is what we do with them: attribution now, training maybe later.

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

**Dropped 2026-09-06, from measured use rather than taste** (the archive records
which tools were actually used):

| dropped | evidence |
|---|---|
| the `train` bulk-correction queue | one correction screen is enough, and #1461 needs the timeline's window-targeted one, not lowest-confidence-first |
| `/api/split` (per-fragment split) | **no caller in the frontend at all** — already dead code |
| manual hide / unhide of a turn | 9 uses, ever: 5 through the train screen's "can't make out", 4 hand-hidden. The other ~52k hidden turns were all hidden by machine |
| the clip-trimmer (boundary nudge) | same family; no use detectable, needed by neither use case |

⚠ **span-assign is HELD OUT of this list, 2026-09-06, and the same measurement is
why.** Its 57 uses across 17 parents on one June day were *all on a hospital meeting*
— use case 2, the half Pippijn ranked first. It is the one gesture behind
reassign/split/merge, i.e. the tool for repairing attribution when diarization merges
two people into one voice, and #1470 records that meeting attribution quality has
never been measured. Cutting the repair tool before knowing whether the thing it
repairs is broken is the wrong order. Revisit once #1470 has ground truth.

⚠ The "zero uses, ever" above was WRONG when first written and is corrected here:
`can't make out (human)` and hand-authored hide reasons exist in the archive. Nine
rows does not change the decision — it changes what the decision may claim.

KEPT for the same reason: **453 of 468 corrections set a SPEAKER** and 183
changed text, so correcting *who spoke* is the job. Hiding a bad CORRECTION stays
(13 real uses) — a mistaken correction otherwise poisons enrolment.

⚠ **And the finding that outranks the list**: corrections ran 456 in June, 12 in
July, and NONE since. See #1467 — whether the review UI is the reason is unknown
and unmeasurable from the archive, and it decides whether #1461 is even the right
next task.

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

  *Reads first, 2026-09-06:* `recalld::reads` serves them from `recall.sqlite`
  opened READ-ONLY — recalld does not own the meaning plane and must not be able
  to write it. Reads went first because a route group that only answers questions
  cannot destroy anything if it is wrong, and because two implementations of one
  contract can be DIFFED.

  ⚠ **They stayed OFF the router until webauth was ported**, and the rule
  generalises: the browsing plane's promise is a Nextcloud sign-in plus a user
  allowlist, so mounting transcripts behind anything weaker — recalld's read side
  takes the sync token — opens a SECOND, WEAKER door to the household's audio. A
  port lands behind the gate or not at all.

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

  *Static serving, 2026-09-06:* `recalld::spa` implements
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

  ⚠ **Mounting it was blocked for a day by dev-lint, and the rule was right.**
  Wiring the SPA made recalld a serving ROOT, so `DL-WIRE-ROUTE-DRIFT` resolved
  its axum table against the frontend's call sites and found **26 calls that would
  miss** — recalld then served 2 of the ~28 `/api/*` routes the app makes, and
  serving the UI would have handed someone a half-working app. It was mounted at
  the cutover, once the proxy could answer for everything not yet ported. The rule
  generalises: do not expose a surface that is not ready, and a lint that can see
  the whole surface is how you find out that it is not.

  *Audio ported and mounted 2026-09-07:* `recalld::audio` serves `/api/audio/{id}`
  and `/api/audio-span` behind the same gate, from the same read-only connection —
  a clip is a read of the meaning plane plus a read of an audio file, so it could
  follow the reads without new authority. The window rules are the port's whole
  content: a rough whole-phrase turn gets a wide context window, a *precise* cutout
  (diarized, or carrying word timings) gets a tight one, because widening that
  would pull in the neighbouring speaker and undo the attribution diarization just
  made. Nine tests pin the arithmetic and that decision; both failure modes are
  silent, since the wrong clip still plays.

  ⚠ `/api/clip` was NOT ported — it had no caller anywhere, having served the
  deleted clip-trimmer. Sizing a route group is the cheapest moment to find that.

  ⚠ ffmpeg and sox are now RUNTIME dependencies of recalld, failing at play time
  rather than at boot. The fleet image already carries them (it once shipped with
  ffmpeg alone and every audio request died inside loudness normalisation while
  transcripts served perfectly), and recalld runs from that same image.

  *The strangler fallback, 2026-09-07 — the change that makes Python deletable
  INCREMENTALLY.* Until now the cutover was all-or-nothing, and that is worth
  naming because it silently governed the whole migration: the browser talks to
  whichever host served the page, so a route ported to recalld changed nothing
  while Python served the app. "Port a group, delete its module" — the only way
  this finishes — was impossible, and every ported group was dead weight until
  the last one landed.

  `recalld::proxy` is the fix: recalld becomes the front door and forwards
  anything it has not ported to the Python beside it in the pod. It is a
  FALLBACK, never an override — it runs only where recalld's own router had no
  match, so a ported route always wins and a half-ported group cannot keep
  silently answering from the old tier. Eight tests, driven against a REAL
  upstream server rather than a mocked client, because the likeliest error in a
  proxy is the request SHAPE and a mock tests one's expectation of it.

  Two properties worth naming, both tested:
  - **The session cookie crosses verbatim.** Otherwise ported routes work while
    proxied ones 401 — a split brain that reads as a webauth bug.
  - **A dead upstream is a 502, never an empty 200.** Mid-migration that is the
    whole diagnosis: "the Python half is down" versus "that route legitimately
    has nothing", and reading the second for the first sends someone hunting a
    data bug that does not exist.

  **THE PORT ARRANGEMENT, and why nothing external moves.** recalld `--bind`
  now REPEATS, and takes BOTH 8001 (what recorders already push to) and 8000
  (what the browser and the registered OAuth redirect already use); the Python
  api moves to a pod-internal 8002 and recalld proxies to it. The hostPort DNATs
  into the pod's shared network namespace, so which container binds 8000 is not
  something Kubernetes polices — which means no recorder is reconfigured, no
  redirect URI is re-registered, and no bookmark changes. The kubes model holds
  one port per container by design and does NOT need changing for this.

  ⚠ **EACH CONTAINER PROBES ITSELF, and that is the whole of it.** The api probes
  `/api/capture` on 8002, recalld probes `/ingest/v1/health` on 8001. The danger a
  shared probe carries is that it passes while the thing it names is dead — a
  probe on 8000 nominally belonging to the api would test recalld, and the pod
  would read healthy with Python down and every unported route 502ing.

  ⚠ Two drafts of this note were wrong before the deploy settled it. One proposed
  probing Python THROUGH the proxy, which stops testing Python the moment recalld
  ports the probed route. The other had the container ROLES swapping — recalld
  becoming the main container. Neither was needed: which container binds 8000 is
  not something Kubernetes polices, so the roles stayed as they were and only the
  probes had to be honest.

  ⚠ **The config change and the image are COUPLED, so they ship together.**
  `--upstream`, `--frontend` and the repeated `--bind` exist only in a freshly
  built binary; landing the kubes change first would leave a deploy that starts a
  recalld which rejects its own arguments.

  **CUT OVER 2026-09-07, and it is live.** recalld serves the app, its own 15
  ported routes and the recorders' ingest; the Python api answers the rest behind
  it on pod-internal 8002. Nothing external moved.

  Three things that only running it revealed:

  - ⚠ **The first deploy reached NONE of the Rust.** recalld mounts its browsing
    plane only when webauth is configured, and the sidecar had only its ingest
    tokens — so `webauth = None`, which means ABSENT rather than open, and every
    ported route fell through to the proxy. The app worked perfectly and not one
    line of the port was exercised. Found by asking WHICH CONTAINER logged the
    request, not by trusting a 200. The keys now come from the same
    `recall-secret` the api reads, which makes the session secret identical by
    construction rather than by remembering.
  - ⚠ **It broke the fleet for about an hour.** The fallback sent `/api/*` to the
    proxy and everything else to the app shell — but Python owns `/sync/*` too,
    so the Mac's sync and jobs agents got `index.html` with a 200 and died on
    `JSONDecodeError`. No archive push, no session pulls, every status green.
    Mitigated by dropping `--frontend` (config only, no image), fixed in
    `proxy::UPSTREAM_PREFIXES`, restored. The regression test only reproduces
    WITH a frontend configured — every prior proxy test ran without one, which is
    exactly why it escaped.
  - Three proxy 502s in the 286 ms before the api finished booting. recalld binds
    and serves before its upstream is ready; expected, and worth knowing so a
    handful of failures right after a deploy is not mistaken for a fault.

  **Where the port stands: on `/api/*`, everything but `/api/sources` is
  recalld's; on `/sync/*`, only the capture handshake is.** That is the durable
  statement; the count behind it changes with every group that moves.
  Re-derive rather than trusting the number, and derive it the way it was
  derived here — from the LIVE app, not by grepping for route strings:

      .venv/bin/python -c "from recall.api import app; \
        print(sorted({(m, r.path) for r in app.routes \
                      for m in getattr(r, 'methods', []) \
                      if str(getattr(r,'path','')).startswith('/api/')}))"

  ⚠ A grep undercounts. It sees a path once where two methods are registered on
  it — `/api/sessions` is both the list (recalld's) and the upload (Python's) —
  and it counts strings that are not routes at all, such as webauth's
  device-exempt entry for `/api/log`, whose route no longer exists.

  That command answers for `/api/*` only. For the front door as a whole, ask from
  the OUTSIDE which tier answered — `server: uvicorn` means Python did, through
  the proxy; recalld sets no `server` header at all:

      for p in /api/capture /api/sources; do
        curl -so /dev/null -D - "http://10.100.0.2:8000$p" \
          | grep -iE '^(HTTP|server:)' | tr -d '\r' | paste -sd' ' -
      done

  ⚠ **That GET probe is USELESS on `/sync/*`, and reads as a confident wrong
  answer.** Those routes are POST-only, so a GET never reaches one: it falls past
  them to the api's SPA catch-all, which returns `index.html` with a 200 and
  `server: uvicorn`. It says "Python answers" before a cutover and after it, for
  the same reason both times, and the reason is not the one being asked about.

  Probe that plane in its real request shape instead — a POST, with a
  deliberately WRONG token. Both tiers reject it identically and BEFORE any
  write, so this changes nothing and still names the answerer:

      curl -si -X POST -H 'Authorization: Bearer not-the-token' \
        -H 'Content-Type: application/json' -d '{"running":true,"pausedUntil":null}' \
        http://10.100.0.2:8000/sync/capture | grep -iE '^(HTTP|server:)'

  Python answers `401` with `server: uvicorn` and a `{"detail": ...}` body;
  recalld answers `401` with no `server` header and a plain-text one.

  The api modules that served a ported group were DELETED with it, not left
  inert — that is the rule the strangler exists to make possible, and `ls
  src/recall/api*` is the list. What moved: reads, playback, the work queue,
  client reports, uploaded meetings including their upload and delete, the
  corrections corpus, the span assign, and the recorders' heartbeats and
  outboxes.

  | group | state |
  |---|---|
  | reads | DONE, including `conversations`, which was the one with logic rather than a query. |
  | audio | DONE. `/api/clip` deleted rather than ported — no caller. |
  | work | DONE (vocabulary, refine) — recalld's first writes. |
  | client reports | DONE. |
  | labels | DONE — correct, turn speaker, correction reassign/hide, span assign. `/api/suggest` and `/voices` were CUT, not ported: voiceprint name suggestions are gone by product decision. |
  | sessions | DONE, including the upload and the delete. ⚠ The delete is the one irreversible operation here and is guarded to UPLOAD sources: the household archive must never be reachable through a path meant for meetings. Every deleted segment is TOMBSTONED in the same transaction, or the Mac's next refine push resurrects the session. |
  | devices | heartbeats and outboxes are DONE. `/api/sources` is NOT: it reads the two-mode liveness model (Mac-local vs fleet) and takes `fleet_capture_state`, so it moves with the capture family or not at all. |
  | capture | DONE 2026-09-08 — status, pause, resume, mounted as ONE group. Splitting the household's control across two languages is the one place a strangler seam is not worth having. |
  | sync | `/sync/capture` DONE 2026-09-08. The other twelve routes are Python's: jobs, labels, the audio blob push and fetch, the segment push and its batch, live turns, and the device/vocabulary reads. |

  *The capture cutover, 2026-09-08, and what it cost to do safely:*

  - ⚠ **The `stateToken` is a hash of the state's JSON**, so it needs Python's
    separators, sorted keys and `null` — and getting it wrong does not fail, it
    silently turns every client's long-poll into a busy poll. It was pinned
    against PRODUCTION rather than against a reading of the code: two of the
    three test vectors are tokens the live fleet served that day, one paused and
    one running.
  - ⚠ **The long-poll re-derives on a slice rather than parking on a notify**,
    which is a deliberate divergence. A notify works in the Python because ONE
    process serves every request; during a cutover the writer may be the other
    tier, whose notify this process cannot receive. And a pause ELAPSING has no
    writer at all, so nothing could ever notify it.
  - ⚠ **The intent keeps its stored SPELLING.** `settled` compares it to the
    Mac's echo by string equality, so re-deriving the timestamp — writing
    `...22.000000+00:00` where Python writes `...22+00:00` — makes a correctly
    applied pause read as transitioning for ever. Caught by a test, not by review.
  - ⚠ **The routes are on the DEVICE-EXEMPT plane** (`webauth::DEVICE_EXEMPT`):
    the mic apps poll `/api/capture` and press pause with no credential at all.
    A gate that demanded a session here would stop every phone's pause button.
  - **Verified by asking WHICH container answered**, with a still-proxied route
    as the control — `server: uvicorn` present on `/sync/*`, absent on
    `/api/capture` — and then by pressing pause and watching the file appear on
    the Mac. A 200 proves nothing here: recalld records INTENT, and the Mac's
    mirror is what actually silences the microphones.
  - **`api_capture.py` was NOT deleted with it.** Fleet images are `:latest`
    only, so a rollback IS a roll-forward; the old implementation is what a
    roll-forward rolls to.

  *The first `/sync/*` route, 2026-09-08 — the plane the one-way peer dials in on:*

  - **`POST /sync/capture` moved with the capture family, and belongs to it.** It
    is the same state under a different door: Isis records intent and cannot dial
    the Mac, so the Mac's mirror POSTs what it applied and reads back what the
    fleet wants, in one round trip. Leaving it on the Python would have split the
    household's capture control across two languages after all.
  - ⚠ **Mounting is gated on `RECALL_SYNC_TOKEN` being in recalld's OWN
    environment**, and absent means the routes are not mounted at all — so
    `/sync/*` keeps reaching Python. That makes shipping the code and cutting
    over to it two separate acts, and makes the rollback a one-line env change
    rather than an image build. Same secret as the api's, same `SYNC_TOKEN` key
    of `recall-secret`; recalld already read it under another name
    (`RECALLD_READ_TOKEN`), which is not a reason to conflate two gates.
  - **Parity was established by RUNNING the Python, not by reading it.** Its
    `record_reported` was called directly for three argument sets and its four
    settings writes diffed against the Rust's; then the route itself was served
    in-process and its status and body compared for the authorised, missing,
    wrong, non-bearer and bad-liveness cases. Both agree, and the expected values
    in `recalld/tests/sync.rs` are the Python's output rather than the Rust's.
  - ⚠ **`json.dumps` preserves the Mac's key ORDER where `serde_json` sorts.**
    `capture_reported_source_liveness` is written by both tiers, so a reordered
    object is a second spelling of one value in one column — which is what makes
    a later parity check report drift that is not drift. `preserve_order` is on
    for this, and the test pins the Mac's order rather than the alphabetical one.
  - **The long-poll costs up to one 2 s slice** where the Python woke in ~RTT, for
    the reason the capture cutover gives above. `GET /api/capture` already ships
    that to every phone in the house, so it is consistency rather than a new
    regression — but an in-process notify layered ON TOP of the slice would buy
    back both, and both routes now have their writer in the same process.

  **CUT OVER 2026-09-08, and it is live.** `RECALL_SYNC_TOKEN` went into recalld's
  container and the Mac's mirror has been handshaking with the Rust since. What
  the cutover cost and how it was checked:

  - **The rollout cost ~40 s of failed handshakes** — five 502s while the api
    container was still booting behind the proxy, then connection-refused, then
    twenty consecutive 200s. The mirror logged each one and retried, which is what
    `run_loop` promises ("a blip must never wedge the mic"). Worth expecting
    rather than diagnosing next time.
  - ⚠ **`/api/sources` is the check that only a live deploy can make.** recalld
    now WRITES `capture_reported_source_liveness` and the Python api READS it, so
    that column crosses the tier boundary every mirror pass. A separator or key
    order this end could not parse the other would return `{}`, and every source
    would show `lastActive: null` — a total loss of the liveness signal that no
    test on either side would catch. All six sources came through the cutover
    byte-identical to their pre-deploy values.
  - ⚠ **The mirror's 5 s cadence cannot tell you the hang works.** Its loop sleeps
    `interval - elapsed`, so a hang that returns instantly and one that holds the
    full wait both produce a 5 s period. Measured instead against the read-only
    twin, `GET /api/capture?wait=&known=`, which shares the mechanism and writes
    nothing: `wait=4` held 4.06 s and `wait=8` held 8.13 s, while a stale `known`
    returned in 0.05 s. Both halves are needed — a hang that never returns early
    is as wrong as one that never hangs.
  - **The Python route is NOT deleted, and the reason is stronger than
    `api_capture.py`'s.** There the old implementation was merely what a
    roll-forward rolls to. Here the rollback IS removing the env var, which
    unmounts the Rust and sends `/sync/capture` back through the proxy — so
    deleting the Python would delete the rollback itself.

  ⚠ **A partially ported PATH needs `method_not_allowed_fallback`.** axum matches
  the path and THEN the method, so with `GET /api/sessions` mounted and no POST,
  a POST is answered 405 by recalld and never reaches the proxy. Porting the list
  would have silently broken meeting uploads with the fallback sitting right
  there. Note what this implies: a proxied method miss does NOT pass recalld's
  gate — it goes upstream unauthenticated and PYTHON's gate refuses it. Python's
  gate is load-bearing, not redundant.

  ⚠ **A route can end up served by NOBODY, and both test suites stay green.**
  `/api/correct` was deleted from the Python in the same change that ported it,
  and the Rust handler was written, tested and never mounted. recalld's tests
  call the function directly (a route test needs the gate mounted), the Python
  suite cannot miss a route it no longer has, and the differential drives the
  function rather than the server. `tests/test_route_coverage.py` now unions the
  axum and FastAPI tables against the frontend's call sites. dev-lint's
  DL-WIRE-ROUTE-DRIFT cannot do this and should not try: it resolves recalld's
  table alone, which is right for a finished port and wrong mid-strangler, where
  "absent from the axum table" legitimately means "Python still serves it".

  ⚠ **Verify a ported WRITE with a write.** The `/api/correct` break survived a
  deploy check that probed only reads.

  *What the differential harnesses caught, kept because the classes recur. Each
  is argued where it bites; this is the index, not the explanation.*

  Three that CORRUPT, and would not have been found by reading:
  - **Character indexing, not byte** (`assign.rs`). Python indexes text by code
    point and the frontend counts UTF-16 units; Rust's `&str` indexes by byte, so
    a cut inside an accented word lands wrong and PANICS. Half this archive is
    Dutch.
  - **Timestamps are passed through, never re-formatted** (`instant.rs`). These
    columns are compared and ORDERED as text, so a re-spelling silently moves a
    row to another page.
  - **A meeting's id is its LOCAL start** (`upload.rs`), and local is
    Europe/London, not the pod's UTC — deriving it from the container clock
    renames every summer recording by an hour.

  One that goes QUIET rather than wrong:
  - **The search index is maintained by the WRITER, not a trigger.** A ported
    write that forgets `transcript_fts` fails nothing and makes its rows
    unfindable by the thing they are most likely looked up with.

  Three about matching Python's stored SPELLING (`pyjson.rs`, `instant.rs`), which
  changes no meaning and is matched anyway, so a later check cannot report drift
  that is not drift: `serde_json` writes no space after `,` and `:` and sorts
  object keys where a dict preserves insertion order (`preserve_order`);
  `json.dumps` escapes non-ASCII; `timedelta` splits whole seconds off before
  rounding half-to-even.

  And one that is not about Python at all:
  - **`serde_json`'s float parser does not round-trip** without
    `float_roundtrip` — it read a stored word timing one ulp low. That applies to
    every float recalld reads from JSON, not only these.
  - **A guard calibrated to a moment rots.** The route-coverage check asserted
    the FastAPI scan saw more than five routes, and failed the moment the port
    passed it. Guard the half parsed from TEXT; the half that reads a live object
    is allowed to reach zero.

  ⚠ **STILL TO DO:** capture — and then the Mac side, which is where the
  remaining Python actually lives.

  ⚠ **The API was never the bulk, so "nearly done" is true of it and false of
  the repo.** Measured 2026-09-07: `find src scripts -name '*.py' | xargs wc -l`
  gives ~20k against ~35k of Rust, and the whole serving tier was about 5% of
  that Python. What remains splits four ways: the Mac's capture/worker/sync
  (~3.2k, portable, `audiod` already covers part), `store.py` + `store_schema.py`
  (~3.3k, the god object of #1340 — Rust already reads and writes this database
  directly, so it is duplication rather than a dependency), `cli.py` +
  `cli_parser.py` (~2.3k, 40 subcommands whose deadness CANNOT be measured
  statically because they are typed at a terminal), and the ML shims (~1.5k),
  which stay Python by §9 and are the intended floor.

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

The floor, permanent: the three model shims (mlx-whisper, pyannote, mlx-lm) —
Python because the models are Python, per [design.md §9](design.md). Plus what
is left of the evaluation side: `wer` and the golden ASR check
(`tests/test_cli_score_asr.py`).

⚠ This used to name `finetune`, `pilot` and `export` in that floor. They were
deleted with the LoRA toolchain ("Training is not a goal" above) and the floor
went on describing them as permanent. A floor is the thing that does not move,
so a deleted module standing in one is the worst place for the claim to rot.

Everything else in `src/recall/` retires with its stage: the mic/streaming
client and relay with C4; worker, live, sync-push, outbox, jobs and
capture-mirror with E3–E4; store, webauth and schemas with the rest of F1. The
authoritative list is `ls src/recall` against this ladder, not a table copied
here; when a stage lands, its deletions land in the same change.

The API modules are already off it — nine went on 2026-09-07, and `health`,
`fleetwatch`, `bounded` and `loss` followed on 2026-09-08 with the doctor (its
own Rust crate, `doctor/`). What is left under `/api` is `api.py` plus
`api_capture`, `api_devices` and `api_models`.

⚠ **`recall.analyse` and `recall.spectrum` are unreachable as of 2026-09-08.**
The speech detector moved to `audiod speech`, using `audiocore::vad` — the same
silero recalld runs, so the Mac and the fleet cannot disagree about what counts
as speech. It had been dead in practice since 2026-07-12: its only trigger was a
cleanup-scan page, so it was on-demand code that stopped being demanded, and
`scan_job.py` was deleted with the F1 session routes before anyone noticed the
output still had readers. They come out once the Rust agent has run unattended.

⚠ **`api_capture` is now DEAD CODE that is deliberately still there.** recalld
serves all three capture routes since 2026-09-08, so nothing reaches it — but
fleet images are `:latest` only, which makes a rollback a roll-forward, and this
is what a roll-forward would roll to. Delete it once the Rust path has survived
real days, the same rule `recall-mic.nix` got on geb.

`/api/sources` is the remaining live route of that family, and it is why
`api_devices` cannot go yet.
