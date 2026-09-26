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

Latency is not a requirement: minutes behind real time is fine, which is what
lets the most accurate models be used.

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
| human corrections | a person listened and typed; the enrolment seed |
| enrolled speakers and voiceprints | seeded from corrections and confirmed turns |
| vocabulary terms | hand-managed proper nouns |

Transcripts, alignments, embeddings and speaker guesses are derived views.
They are versioned, never overwritten: a better pass supersedes or hides, and
the history stays. Re-transcription is on demand, triggered by a measured win
on the golden ASR check, never by a calendar.

## Two planes, two databases

`ingest.sqlite` is the **audio plane**: one row per delivered blob, speech
seconds per blob, the job queue and the passes' ledger, and the room stream's
history (levels, room blocks, room jobs). `recall.sqlite` is the **meaning plane**: sources, audio segments,
turns, corrections, speakers and voiceprints, and the FTS index. recalld owns
both; the schema of the second is a migration ladder
(`recalld::meaning_schema`), that of the first one `ensure`
(`recalld::ingest_schema`). Every instant in the meaning plane is text in one
spelling (`audiocore::instant`), because instants are compared and ordered as
text. Writers take it as a `Stamp`, which only a real instant builds, and
triggers (v47) refuse any other spelling. Foreign keys are enforced.

Every write to the turn table goes through `recalld::turn_store`, a test fails
the build otherwise. It owns the typed provenance, the stage the app shows
(live, transcribed, diarized, corrected) and the rule for what a person owns:
a turn they corrected or named. No pass hides such a turn or writes over its
span.

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
open; `PUT /ingest/v1/segments/{source}/{filename}` with the source's bearer
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

## recalld

One binary (axum, rusqlite) on Isis, binding the ingest port the recorders
push to and the port the browser uses. It owns:

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
  uploaded meeting, `enroll-speaker` for turns a person has named. Enrolment outranks capture time in the lease, or a label would wait
  behind days of backlog.
- **The passes.** `turns` writes the transcript of a clip that has none;
  `diarized` aligns the words to the speaker spans and labels the turns that
  exist, or replaces them with speaker-split ones, and never writes fewer turns
  than it hides; `enrol` turns a named turn into a voiceprint; `rematch`
  re-derives speaker guesses when the voiceprints have grown. Every terminal
  decision that writes nothing leaves a ledger row, or the clip sits at the
  head of the queue for ever. Every row a pass writes carries a provenance that
  names the pass, so a pass can be taken back. Job kinds are one enum
  (`audiocore::job::Kind`) shared with the runner.
- **The browsing API and the app.** Timeline, search, playback, corrections,
  sessions, labels, the capture control, behind the Nextcloud sign-in. The
  frontend's types are generated from the route structs (`ts-rs`); the gate
  fails on drift.

Quality rules run where rows are written: a repetition loop or a wordless turn
is refused at the write (`audiocore::text`, shared with the doctor so both
judge the same text the same way); so is a phrase Whisper writes over
silence ("Thank you.", video sign-offs) where the clip heard no speech: none
inside the line's own span, or under a second in the whole minute
(`recalld::quality::Heard::invented`); a whole-clip
language outside the
household's two zeroes a turn's confidence rather than hiding it. Confidence,
length and the language label alone are never grounds to hide: the commonest
low-confidence turns are quiet real agreement, and most turns labelled a
foreign language are Dutch and English mislabelled.

### Text is written once

A direction, not yet the whole rule: a pass that attributes may split a turn
or label it, and may not rewrite its text to do so. Every serious data loss here
was a metadata pass that hid text to deliver a label. What ships: a pass that
would write fewer turns than it hides labels instead (`Swap::Attribute`), and a
proven, unwired split (`diarized::split_at_speaker_changes`). `Swap::Replace`
still exists for the rest.

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

`recall-live` is the instant feed: it reads the tap the segmenter publishes,
cuts at pauses with the same detector, transcribes each utterance as the
speaker stops, and pushes it to `POST /sync/live`. A live turn is provisional;
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
through the app and the household capture cannot. odin pulls a nightly restic
of Isis (a SQLite snapshot plus the audio tree); the Mac keeps the protected
master archive on an encrypted volume and delivers every closed segment to
Isis.

## Decisions that bind

- **Combining microphones lost; selecting the best one tied it.** On 38
  corrections, the best single mic scored 0.229 median WER against 0.348 and
  0.437 for two fusion arms, 14 worse against 4 better; a control showed the
  pipeline itself cost nothing. The room stream is selection. It is an
  experiment run by hand on a copy of the data (`experimental/room`, #1388),
  not part of production.
- **Enhance the selected mic, do not stitch mics.** By ear, the best microphone
  through DeepFilterNet was the clearest version of every minute tried.
- **Denoising hurts far-field ASR.** Two denoisers measured worse; raw is best.
  The quality lever is more and closer microphones.
- **Never transcribe short isolated clips.** Whisper needs context or it
  hallucinates and mis-detects the language. Pause-bounded utterances are not
  short isolated clips; fragments that begin or end mid-speech are.
- **Training is not a goal.** Correct, enrol; what to train from that is a
  later decision. The LoRA toolchain is deleted.
- **Ask and summaries are cut.** The archive is searchable, not answerable.
  `llm-host` stays on the Mac for `life`'s emotion worker, not for recall.
- **Keep everything.** Audio scope is answered with disk, not by discarding;
  trimming would bake today's speech detector into the archive.

## What is Python

The two model shims and their wrappers (`asr`, `diarize`, `speakerid`), the
golden ASR check (`score_asr`, `wer`), and `llm-host`. Nothing else, and that is
the end state: the models are Python, so their wrappers are. Each is its own
module, run as `python -m recall.<module>`.
