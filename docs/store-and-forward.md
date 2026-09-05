# Store-and-forward capture — the proposal, and what it has to answer

**Status: proposed, not built.** Nothing here is running. This file exists so the
questions get answered before code is written, because two of them are one-way
doors: the answer decides what is *possible* later, not merely convenient.

## The shape proposed

1. **Phase 1 — record and deliver, nothing else.** Recorders own their audio:
   Rust on the machines, Kotlin/Swift on the phones. Each records verbatim,
   **caches locally**, uploads to Isis, and deletes its local copy only once Isis
   is known to hold it. No transcription anywhere in this phase.
2. **Phase 2 — one stream, pulled.** The per-microphone audio is processed into a
   single high-quality sequence for the transcriber. The Mac is unreachable from
   outside, so it **fetches work from, and pushes results to, a simple HTTP
   service on Isis**. The transcriber stays Python, because it is the part that
   calls the model.

## What this changes in the live system

Read these before answering anything below; each is a decision that was made
deliberately and would be reversed here.

- **Phone ingest was deliberately kept on the Mac** ([isis-migration.md](isis-migration.md),
  decision 5). The reasoning was that the PCM has to reach the Mac anyway and
  Isis cannot dial it, so Isis-side ingest would buy only "phone capture survives
  a Mac outage" — and that outage is visible rather than silent. Phase 1 reverses
  this, for a different reason than the one that was weighed: delivery guarantees,
  not availability.
- **The Mac holds the protected master archive** and refuses destructive orders
  from Isis (the sweep veto, [isis-migration.md](isis-migration.md)). Deleting
  locally after upload removes the thing that veto protects.
- **Recorders discard while disconnected today**, on purpose
  ([devices.md](devices.md)): the server rebases a connection's segment names by
  one offset measured at its first byte, so a replayed backlog would be stamped
  correctly at its head and progressively wrong toward its tail. That doc already
  states the fix phase 1 needs — *a protocol that times each segment, not a bigger
  buffer.*
- **Combination measured worse than choosing** ([audio-plane.md](audio-plane.md)):
  SNR-weighted fusion lost its WER gate, and fusing two comparable microphones was
  a null result. "Produce a single high-quality sequence" currently has no method
  that beats carrying the best source.

## The questions

### 1. What format is cached and uploaded? (one-way door)

Opus is not phase-linear. If the recorders ship Opus, the phase relationships
between microphones are destroyed at the source and phase 2 **permanently** loses
both coherent combination (beamforming) and the spatial TDOA features that
person-filtered streams depend on ([audio-plane.md](audio-plane.md), stage 4).
Lossless keeps both and costs roughly an order of magnitude more.

Sizes, from this archive rather than estimated: five sources produced 104 MB in
2 h 22 min of recording on 2026-09-05, so continuous capture is on the order of
1 GB/day and Isis's measured free space holds a few years of it. Lossless at
48 kHz is on the order of 20 GB/day, which is weeks, not years. (Re-measure both
rather than trusting these; the method is `du` over the source directories and
`df` on Isis.)

So: lossy forever, lossless forever with more disk, or lossless for a short
retention window with Opus for the long tail?

### 2. What replaces the sweep veto? (one-way door)

The threat this fleet defends against is **destruction, not observation**. The
one-way VPN plus the Mac's local evidence check means a compromised Isis can, at
worst, delete audio the Mac has itself measured as an empty room
([isis-migration.md](isis-migration.md)). Phase 1 deletes the local copy on Isis's
say-so, which makes Isis's word sufficient to destroy audio — the exact authority
the veto exists to deny.

Note this is not a total regression: odin pulls a nightly restic backup of Isis and
the Mac keeps an off-site copy ([running.md](running.md)). But that leaves a window
between upload and the next backup in which one machine holds the only copy, and it
makes the answer to question 3 load-bearing.

### 3. What does "Isis has it" mean, and may a recorder auto-delete at all?

This repo has already litigated this once, for the meeting recorder
([meeting-recorder.md](meeting-recorder.md)), and reached the opposite conclusion
to phase 1:

- **A 2xx is not proof.** A post cut short mid-stream still parses and still
  returns success with minutes missing off the end. So the phone compares the
  server's reported length against the file it still holds, and a mismatch — *or
  an unreadable length on either side* — lands in `unverified/`.
- **Nothing is deleted automatically, not even after a verified upload.** That was
  tried the other way first and judged wrong; deleting is the one thing only a
  person does.

Continuous capture cannot keep every segment on a phone for ever, so the answer
here must differ — but it should differ deliberately. Is the receipt a checksum, a
duration comparison, or a later "and odin has it too"? And is the deletion trigger
a confirmation, or a cache ceiling?

### 4. Does phase 1 include per-segment timing?

The stamping fix is the precondition for holding audio across a disconnect
(see above, and #1407). Without it a spooled backlog uploads with names that drift
further from the truth the longer the outage was, and cross-microphone alignment —
everything phase 2 does — reads those names. Is this in phase 1's scope, or does
phase 1 ship with connection-stamping and inherit the drift?

### 5. Who produces the single stream, and on what hardware?

Isis is 4 cores, 15 GB RAM shared with Nextcloud, and has no GPU
([isis-migration.md](isis-migration.md)). The Mac is where the compute is. If Isis
combines, it needs the headroom; if the Mac combines, it fetches every microphone's
audio back over HTTP and the "single sequence" is produced by the same machine that
transcribes it — in which case the service on Isis is a blob store and a queue, and
combination is not really phase 2's boundary.

Second half of the same question: **combine into one, or choose one?** What is
measured today is that choosing beats combining, and that choosing by speech level
needs per-device calibration to mean anything ([audio-plane.md](audio-plane.md)).

### 6. Does the USB mic keep our code out of its path?

Local capture is sox → ffmpeg with nothing of ours in between, because a real-time
device has no buffer to absorb a stall; and the recorder runs
`ProcessType = Interactive` because the throttled class was measured dropping
roughly half the wall clock ([design.md](design.md) §7, [devices.md](devices.md)).
A Rust recorder that also caches, hashes and uploads must keep all of that off the
capture thread. Is the Mac's own microphone even in phase 1's scope, or does it
stay as it is?

### 7. Retention — the open question this forces

[design.md](design.md) §10 has retention listed as open (forever vs rolling
window). Phase 1 cannot be built without answering it: "delete locally once the
server has it" needs to say what the server then does, and for how long.

### 8. Does live transcription survive phase 1?

"Nothing needs to transcribe it at this point" is coherent with the design —
latency is explicitly not a requirement ([design.md](design.md) §1). But the live
path exists and is used to answer *what are they saying now* (#1383). Is it dropped
during phase 1, kept running on the old path alongside, or retired for good?

### 9. Does the existing archive and its corrections come along?

The human corrections are the only ground truth the system has: the fine-tune
corpus, the enrolment seed, and the WER oracle every gate is measured against
([pipeline.md](pipeline.md) §5). Without them nothing in phase 2 can be gated, and
the denoising and fusion results both show what shipping an unmeasured "better"
signal costs.

### 10. Which credential do continuous uploads carry?

The token planes are deliberately narrow ([isis-migration.md](isis-migration.md)):
`/sync/*` has the Mac's sync token, and `POST /api/sessions` accepts a device token
**and nowhere else**, because a phone is easier to lose than a Mac and that closed
path set is what stops a phone that can upload from being able to read the
household's transcripts. Continuous upload from every recorder is a new path; it
needs a plane, and the reasoning above says which properties that plane must have.
