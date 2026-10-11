# recall — how it is built

A local, always-on system that records household speech, transcribes it,
attributes it to the person who said it, and makes it searchable: a memory aid,
a faithful record of what was said, by whom, when. Everyone entering the house is
told they are recorded. This file is the shape of the system as it runs;
[running.md](running.md) is how to operate it.

## Requirements, in priority order

1. **Completeness.** Never silently drop audio; a gap is the worst failure. Raw
   audio is kept, so any minute can be re-derived later.
2. **Accuracy.** Proper nouns (people, places, recurring topics) must be right.
3. **Attribution.** Every utterance tagged with who said it, with an "unknown"
   bucket for visitors.
4. **Recall.** Full-text search, time and speaker filtering.
5. **Privacy.** On-device, encrypted at rest, no cloud ASR, no telemetry. The
   repository holds no transcript content and no names; those live only in
   runtime data.
6. **Low maintenance.** Runs as services, restarts on failure, surfaces its
   health.

Text must be there within minutes, even if not yet perfect; the accurate
version may follow later. Real-time is not needed, which is what lets the most
accurate models do the archive pass.

## What it is for

Two situations, and nothing else. Anything that serves neither is weight.

1. **The home room, recorded by several microphones at once.** Continuous
   capture, several mics hearing the same speech, turned into one searchable,
   attributed record.
2. **A single recording of a meeting, in hospital.** One file from a phone,
   uploaded, transcribed and diarized, read back as a clean attributed
   transcript. Not continuous, not multi-mic; the accuracy that matters is
   proper nouns and medical terms.

The two share a spine (capture, ASR, diarize, attribute, read) and differ in
almost everything else.

## The shape

```
phones (Kotlin/Swift)      geb + machines (audiod)      Mac USB mic (audiod)
   each records CLOSED segments locally, capture-stamped, cached on device
        └─────────────┬── PUT segment, sha-256 receipt ──┬─────────┘
                      ▼                                  ▼
 ┌─ Isis — recalld (Rust): the system of record ──────────────────────────┐
 │  ingest plane: append-only blob store + ingest.sqlite  (no delete      │
 │  speech detection at ingest → evidence, liveness        endpoint      │
 │  work queue → jobs out, results in → turns               exists)      │
 │  browsing API + Nextcloud sign-in + the Angular app                   │
 └──────────────┬──────────────────────────────▲──────────────────────────┘
      odin restic nightly              the Mac POLLS (one-way WireGuard)
                      ┌────────────────────────┘
 Mac = stateless GPU worker: `runner` (Rust) polling the queue, driving two
 Python model shims — mlx-whisper (`asr`) and pyannote (`voices`)
```

Principles:

1. **Recorders own their audio until eviction.** Record, cache, upload, verify
   the receipt, keep anyway until local cache pressure evicts the oldest
   verified segment. No recorder deletes because a server said so.
2. **Isis is the system of record and the only always-on service.** One Rust
   daemon, `recalld`, owns the ingest plane, the queue, the passes that
   write turns, and the browsing API.
3. **The Mac is a stateless GPU worker.** If it dies, every other recorder
   keeps recording and delivering; the loss is its own microphone going
   forward plus its own unuploaded cache.
4. **The one-way VPN is untouched.** Recorders push to Isis; the Mac polls
   Isis; nothing initiates toward the Mac.
5. **The ingest plane is append-only.** No network surface deletes; destruction
   is an operator act behind the backup chain.
6. **Python survives only where a model is called.**

Why this shape: the ML is Apple-Silicon-bound (mlx-whisper is Metal-only,
pyannote crawls on CPU) and nothing else is; combining microphones lost to
selecting the best one; and the fleet's threat model is destruction, not
observation, so the Mac's isolation is the backbone rather than an obstacle.

## What must survive

Only the recording has to survive. Everything derived may be recomputed.

| system of record | why it cannot be recomputed |
|---|---|
| the audio | requirement 1 |
| the edit log | what people did: words, names, nobody spoke, left out, voice names, languages |
| enrolled speakers and voiceprints | learnt from the stretches people named |
| vocabulary terms | hand-managed proper nouns |

Model output is derived: each clip keeps its current transcription and
diarization, and a re-run replaces them. The lines shown are a function of
them, the edit log and the voiceprints (`transcript::render`), cached per clip
in `rendered_lines` and rebuilt when an input changes. Nothing a person did is
stored on a line, so a new result never overwrites it. Re-transcription is on
demand, triggered by a measured win on the golden ASR check, never by a
calendar.

## Two planes, two databases

`ingest.sqlite` is the **audio plane**: one row per delivered blob, speech
seconds per blob, the job queue and the passes' ledger, and the retired room
stream's tables. `recall.sqlite` is the **meaning plane**: sources, audio
segments, the edit log, the rendered lines and their search index, the live
feed's lines, speakers and voiceprints. Nothing reads the tables the rendered
view replaced; they stay until a backup cycle has them. recalld owns
both; the schema of the second is a migration ladder
(`recalld::meaning_schema`), that of the first one `ensure`
(`recalld::ingest_schema`). Every instant in the meaning plane is text in one
spelling (`audiocore::instant`), because instants are compared and ordered as
text. Writers take it as a `Stamp`, which only a real instant builds, and
triggers (v47) refuse any other spelling. Foreign keys are enforced.

**A clip is a stored file** (#1911): `clips` in `ingest.sqlite` gives each one
an id, its source, its start in integer microseconds, and its path relative to
the data root. The database keeps the invariant itself: a trigger gives every
file stored a clip in the same transaction, and a rename keeps the id. The file
is the key, never `(source, start)`: a phone's own copy and the Mac's cut of its
stream often open in the same second. The types (`ClipId`, `SourceId`,
`Instant`, `Span`) live in the `transcript` crate, which may do no IO
(`scripts/check_pure_crate.py`).

Lines are made in one place, `transcript::render`, and stored in one,
`recalld::view`. What a person did is an act in `edits` (`recalld::edit_log`),
append-only: a change of mind is a later act or a retraction. A person's act
owns its span; the model's words under it give way, kept as hidden rows with
the act that dropped them, so taking the act back shows them again.

A recording machine keeps no database of meaning. Beside its segments,
`audiod` appends the capture log (`capture-events.jsonl`, the format in
`audiocore::capture_log`): each source registering and its kind, phones
connecting and dropping, pauses and resumes. The doctor reads that log and the
files; what the fleet measures (speech per microphone, the live tier) it asks
the fleet for. The Mac's old `recall.sqlite` is retired as an archive, read
only by the doctor's volume probe.

## Recorders

Three implementations, one contract.

| recorder | capture | delivery |
|---|---|---|
| Mac USB mic | `audiod capture`: sox on CoreAudio, ffmpeg segmenting | `audiod upload` |
| Linux hosts (geb) | `audiod capture` with ffmpeg on ALSA | the same |
| phones | Kotlin and Swift apps: stream PCM to `audiod ingest`, and record closed segments locally | the apps' own uploader |

The contract: record fixed-length segments (60 s) named
`<source>-YYYYMMDDTHHMMSS.<ext>` from the recorder's own UTC clock at segment
open (a phone's own copy is `.phone.flac`, because `audiod ingest` also cuts its
stream and the two often open in the same second); `PUT /ingest/v1/segments/{source}/{filename}` with the source's bearer
token; compare the receipt's sha-256 with a local re-hash before counting it
delivered; evict only under cache pressure, only verified segments, oldest
first, never the open one; honour the household pause. The name is the only
timing a segment carries, so one crate parses it (`audiocore::names`), and a
skewed clock is recorded beside the receive time rather than refused.

Capture is sox into ffmpeg, never ffmpeg's own device input: ffmpeg's
`avfoundation` drops samples on this Mac. The USB mic is pinned by CoreAudio
device name; an unknown name fails loudly rather than falling back to whatever
macOS made the default. The capture agents run at launchd's `Interactive`
class, because `Background` is throttled and a starved real-time reader drops
samples no later pass recovers. A pause stops every recorder; nothing is
recorded against one.

Segments are FLAC, lossless, kept forever. Opus was the default once and
destroys phase, which is why the older archive cannot be combined coherently.
The phone apps wrote their own copy as WAV until October 2026;
`recalld phone-flac` converts those, keeping a FLAC only if it decodes to the
WAV's exact samples.

## recalld

One binary (axum, rusqlite) on Isis, on one port behind the front door for
recorders, the Mac and the browser. It owns:

- **The ingest door.** Bytes stream to a temp file, are fsynced, renamed into
  place, the directory fsynced, the row inserted, then the receipt goes out.
  A re-PUT of identical bytes is idempotent; a different blob under a taken
  name is 409 and the stored one is untouched.
- **Speech evidence per blob.** Silero (`audiocore::vad`, the same detector the
  Mac runs) measures speech seconds, and where they fall (`regions`). A
  segment gets a transcription job only once measured, and none if measured
  silent: transcribing silence returns inventions ("Thank you."), not nothing.
  Rows from before `regions` existed are backfilled with whatever room a batch
  has left after new clips.
- **The work queue.** Jobs are derived from the blobs, never enqueued, so a
  missed enqueue cannot strand audio. A lease is time-bounded; a runner that
  dies lets it lapse; a job nobody finishes is retired after three leases. Kinds:
  `transcribe-segment` and `diarize-segment` for every microphone clip and
  uploaded meeting, `enroll-speaker` for stretches a person has named.
  Enrolment outranks capture time in the lease, or a label would wait
  behind days of backlog.
- **The passes.** `view` renders every clip whose inputs moved (its results,
  its speech measurement, the newest act on it or its session, the
  voiceprints), and every clip once at start, since render's rules may have
  changed with the binary; a person's act renders its clip before the reply.
  `enrol` turns a person's naming into a voiceprint and forgets one whose
  naming was taken back or renamed. `registrar` gives a microphone clip the
  audio row its lines hang from. Every terminal decision that writes nothing
  leaves a ledger row, or the clip sits at the head of the queue for ever. Job
  kinds are one enum (`audiocore::job::Kind`) shared with the runner.
- **The browsing API and the app.** Timeline, search, playback, corrections,
  sessions, labels, the capture control, behind the Nextcloud sign-in. The
  frontend's types are generated from the route structs (`ts-rs`); the gate
  fails on drift.

Quality rules are render's: a repetition loop, wordless text, or a phrase
Whisper writes over silence ("Thank you.", video sign-offs) where the clip
heard no speech (none inside its own span, or under a second in the whole
minute) is left out, kept as a hidden row with its reason (`transcript::text`,
`transcript::quality`). A whole-clip language outside the household's two zeroes a line's
confidence rather than hiding it. Confidence,
length and the language label alone are never grounds to hide: the commonest
low-confidence turns are quiet real agreement, and most turns labelled a
foreign language are Dutch and English mislabelled.

### A sentence breaks where a person said so

Inside a model segment a sentence goes to whoever speaks most of it: a
diarized boundary is approximate to a word, so a change inside a sentence is
jitter. A person's naming is not: where they named part of a sentence, it
breaks there.

### Transcribing a clip again

A clip whose words are wrong (a Whisper loop) is asked for again by name
(`retranscribe`, `recall-cli retranscribe`): its transcription goes back in the
queue keeping its result, so the clip shows its old words until the new ones
land and render replaces them; a person's acts stand over either. A request
still waiting can be taken back. A finished one cannot: only the current
result is kept.

### Speaker attribution: identification, not diarization

For the household, the question per stretch of speech is which of a few known
people it is, not how many voices there are. Whisper's own segment boundaries
fall at speaker changes nearly every time; pyannote's spans cover about a fifth
of the speech in a busy clip. So the direction (#1711) is to embed each Whisper
segment, match it to the enrolled prints, and use cross-mic energy as a
position prior, with a small model fitted on the household's labelled turns.
Pyannote's clustering stays for meetings: unknown speakers, one microphone.

## runner and the model shims

`runner` is the Mac's whole orchestration: lease a job, fetch the blob, drive a
shim over stdio, push the result, ack. It holds no state. Two agents run it,
one per shim, because each shim holds one model's weights. A shim is a
long-lived Python process speaking line-delimited JSON on stdio, holding
weights, taking one job at a time, doing no I/O beyond its stdio and the audio
path it is handed. `asr` wraps mlx-whisper (large-v3-turbo, word timings);
`voices` wraps pyannote (diarization and embeddings). The vocabulary the
transcriber is biased with is read from the fleet at startup and handed to the
shim per job; a runner that cannot read it refuses to transcribe unbiased.
The transcribing runner also serves messages' queue of voice messages
(`--messages-url`), one job whenever recall's queue is empty, so one set of
Whisper weights serves both. Those jobs carry messages' own token, get no
vocabulary, stamp no pulse and use their own scratch directory; a failure of
messages' server is logged and skipped, never recall's backoff.
Every message between the runner and a shim is defined once, in
`audiocore::shim`: the runner builds requests from it, refuses a reply that does
not read as it, and stores the reply as the shim sent it; the fleet reads stored
results through the same types. `tests/fixtures/shim/` holds one example of each
message, which the Python shims' tests must produce and accept and the Rust
tests must read and write, so a shape changed on one side alone fails a test.
mlx-whisper's temperature fallback is patched to draw fresh noise per token
(`recall.asr.unfreeze_sampling`): as shipped, its compiled sampler froze one
random key per process, and the fallback meant to escape a repetition loop made
them (#1764).

`recall-live` is the instant feed. It keeps "within minutes" true while the
archive pass runs 3 to 25 minutes behind (median per day, late September), for
the USB mic only: it reads the tap the segmenter publishes, cuts at pauses with
the same detector, transcribes each utterance as the speaker stops, and pushes
it to `POST /sync/live`. A live turn is provisional;
the archive pass hides it when it writes the same minute. It joins whatever is
waiting into one call, because Whisper pads every call to 30 seconds and the
cost is the window, not the audio.

## Credential planes

| plane | credential | can |
|---|---|---|
| browsing | Nextcloud SSO session | read and write the app's API |
| recording control | none (network-gated) | pause state, liveness, heartbeats |
| device upload | `RECALL_DEVICE_TOKEN` | `POST /api/sessions` only |
| ingest | per-device token | `PUT` its own source's segments; nothing else |
| sync | `RECALL_SYNC_TOKEN` | the Mac's reads: the queue, blobs, the vocabulary prompt, the live tier's numbers, and the capture handshake |

Unconfigured means open for dev and tests, except the browsing and sync planes,
which are not mounted at all without their credential: they carry the
household's transcripts and its pause control. The Mac is a one-way WireGuard
peer: it may initiate into the fleet and nothing may initiate toward it, so
every cross-machine exchange is Mac-initiated, and a pause pressed in the web
UI reaches the microphone by the Mac's mirror long-polling for it.

## Deletion authority

No network path deletes. The ingest plane has no delete endpoint; recorders
evict only under their own cache pressure; an uploaded meeting can be deleted
through the app and the household capture cannot. Deleting a meeting removes
it everywhere: its audio, its lines, and its rows in the ingest plane, whose
jobs hold its transcription. `sessions::DeletedUpload` is the only key to that
last step, so household capture cannot reach it. odin pulls a nightly restic
of Isis (a SQLite snapshot plus the audio tree); the Mac keeps the protected
master archive on an encrypted volume and delivers every closed segment to
Isis.

## Decisions that bind

- **Continuous capture is home-only; outside, a meeting is recorded on
  purpose.** A phone's mic opens only while it reaches the Mac on the home
  network, so that connection is also how a phone knows it is home. Away, the
  app's meeting recorder is the tool (decided 2026-10-03, superseding
  2026-09-06's "record whenever unpaused"). So the stream stays, and a phone
  minute is kept twice (the Mac's cut and the phone's own copy, which also
  covers a dropped stream).
- **Combining microphones lost; selecting the best one tied it.** On 38
  corrections, the best single mic scored 0.229 median WER against 0.348 and
  0.437 for two fusion arms, 14 worse against 4 better; a control showed the
  pipeline itself cost nothing. Transcribing the selected mic once (the room
  stream) then added nothing over each mic's own transcript (#1388), so it was
  removed; git history has `experimental/room`.
- **Enhance the selected mic, do not stitch mics.** By ear, the best microphone
  through DeepFilterNet was the clearest version of every minute tried.
- **Denoising hurts far-field ASR.** Two denoisers measured worse; raw is best.
  The quality lever is more and closer microphones.
- **A minute is decoded whole, in the language Whisper guesses for it.**
  Splitting a minute where per-stretch language guesses change was built and
  removed (#1388). On played speech switching language every turn it cut word
  errors from 47% to 7%; on household audio Whisper's guess on short far-field
  stretches leans English, and where the guesses disagreed with the whole
  minute, forced-decode confidence showed the speech Dutch in 13 of 17. Real
  mixing is rare at home. The room stream, scored the same way
  (`experimental/playback`), matched the best microphone and never beat it.
- **Never transcribe short isolated clips.** Whisper needs context or it
  hallucinates and mis-detects the language. Pause-bounded utterances are not
  short isolated clips; fragments that begin or end mid-speech are.
- **Training is not a goal.** Correct, enrol; what to train from that is a
  later decision. The LoRA toolchain is deleted.
- **Ask and summaries are cut.** The archive is searchable, not answerable;
  recall holds no LLM.
- **Keep everything.** Audio scope is answered with disk, not by discarding;
  trimming would bake today's speech detector into the archive.

## What is Python

The two model shims and their wrappers (`asr`, `diarize`, `speakerid`), the
golden ASR check (`score_asr`, `wer`). Nothing else, and that is
the end state: the models are Python, so their wrappers are. Each is its own
module, run as `python -m recall.<module>`.
