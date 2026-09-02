# recall — household speech recall system

## 1. Purpose

A local, always-on system that records, transcribes, and attributes household
speech and makes it searchable. A **memory aid**: a faithful, attributed,
searchable record of what was said, by whom, when — not generic captioning.
Everyone entering the house is told they are recorded.

**Hard requirements, in priority order:**

1. **Completeness** — never silently drop audio; a gap is the worst failure. Raw
   audio is retained so any segment can be re-transcribed later.
2. **Accuracy** — proper nouns (people, places, recurring topics) must be right.
3. **Attribution** — every utterance tagged with who said it (named members, plus
   an "unknown" bucket for visitors).
4. **Recall** — full-text search, time/speaker filtering, later summaries + Q&A.
5. **Privacy** — 100% on-device, encrypted at rest, no cloud ASR, no telemetry.
6. **Low maintenance** — runs as a service, restarts on failure, surfaces health.

Latency is **not** a requirement — batch processing minutes or hours behind real
time is fine, which frees us to use the most accurate models.

## 2. Hardware

Mac mini **M4**, 32 GB, macOS 26.5 — runs Whisper large-v3-turbo (Metal) plus
diarization faster than real time. Input: USB condenser mic (CoreAudio, 48 kHz)
via sox, downmixed to 16 kHz mono for ASR. macOS mic permission must be granted
to the capture process (and each launchd agent that opens the mic).

## 3. Architecture

```
USB mic / phones ─► capture (sox → ffmpeg segments) ─► raw audio store (Opus, immutable)
                                                              │
              VAD ─► ASR (Whisper) ─► diarization (pyannote) ─► speaker ID
                                                              │
                                          SQLite + FTS5 — versioned transcripts
                                                              │
                                          search / browse / (later) summarise + Q&A
```

Each stage is decoupled: **capture never blocks on transcription.** If the worker
falls behind or crashes, raw audio keeps accumulating and is processed when it
catches up — this is what guarantees requirement #1.

## 4. Precision in layers, not one model

Fine-tuning is the **last** lever, not the first: it needs a corpus of corrected
household audio that doesn't exist until the system has run a while, and the
biggest errors are proper nouns and speaker confusion, which fine-tuning doesn't
fix efficiently.

| Lever | Effort | Fixes | Status |
|---|---|---|---|
| Whisper large-v3-turbo base | none | general accuracy | **built** |
| Vocab biasing (`initial_prompt` + names) | trivial | proper nouns | **built** |
| Diarization + speaker enrolment | low | attribution | **built** |
| Post-correction dictionary | low | systematic errors | **refuted 2026-09-02** |
| LoRA fine-tune on corrections | high | accent/acoustic residue | trained, **not deployed** (`adapter-20260708b`) |

"Training on the actual people" is delivered mainly by **enrolment** (lightweight
voiceprints), not retraining. The LoRA retrain is the heavy lever, and **nothing runs
it today**: two adapters were held back by the A/B gate (truncation, then a
language-head bug), the third rode the idle refine pass 2026-07-09..07-11 and was
pulled — once windowing made it correct on long audio it was **~8x slower** there (a
32-layer fp32 decoder against turbo's 4) for a WER win only ever measured on short
clips. Refine's precision is the diarization and word alignment, not the ASR model, so
it stays on turbo; the re-enable arguments sit commented in `deploy/hm-agents.nix` and
only an A/B win on real audio should uncomment them (see [pipeline.md §5](pipeline.md)).
The post-correction dictionary was the cheap lever still un-built — until the
corrections corpus was mined for it (2026-09-02, 176 changed pairs, difflib
word alignment): only 9 substitutions recur at all, and they split into
proper-noun garbles (each garbling different, so exact-match replacement
barely fires — and vocabulary biasing is the built lever for exactly those)
and context-dependent hearing errors (EN word ↔ NL word, pronoun swaps) that
auto-replacement would corrupt. A dictionary would have touched ~15 archive
turns. Verdict: not worth its machinery at this corpus size; re-run the
mining if the corpus grows several-fold, and route recurring garbles into
vocabulary terms instead.

## 5. Components

**5.1 Capture.** sox `-d` (CoreAudio) produces the raw PCM; ffmpeg's `segment`
muxer writes a continuous ring of 60 s Opus files (32 kbps VoIP), UTC-named and
gap-free — a crash loses ≤1 segment. ffmpeg's own `avfoundation` input dropped
~20% of samples on this machine, hence sox as the device front-end. 48 kHz
captured; a 16 kHz mono copy is derived for ASR. ~110 GB/year continuous, fits
the backup volume.

**5.1a Multiple mics.** Spare phones (Android + iOS) running the `recall-mic` app stream
raw PCM over TCP to one shared ingest port and are segmented exactly like the USB
mic; the USB mic is the always-on baseline, phones are best-effort. Co-located
mics hearing the same speech are folded into one *moment* at recall time (best
source shown, others kept as alternates). Raw per-source audio is retained, so
richer offline fusion (drift-correct + blind beamforming/GSS for overlapping
speech) can be added later — **designed, not built.** Connect/identity/liveness:
[devices.md](devices.md).
> **Known limitation:** phone segments are *arrival*-stamped, not capture-stamped,
> so they lag the USB mic by a variable buffering offset — cross-mic timestamps
> don't share a clock. No audio is lost; only cross-mic alignment is affected.
> Clean fix (phone sends its capture epoch) is deferred until a feature needs it.

**5.2 VAD.** Silero gates transcription to speech spans (Whisper hallucinates
filler on silence). Raw audio is kept regardless, so a better VAD can re-derive.

**5.3 ASR.** mlx-whisper, `large-v3-turbo` (`asr.DEFAULT_MODEL`), on every pass.
Word timestamps in the refine pass align words to diarized speakers. Non-turbo
`initial_prompt` vocab biasing is built: every transcription pass is biased by
the household vocabulary (enrolled names + terms managed on the Labels page,
`recall.vocabulary`), rebuilt per call so a new term applies immediately.
Non-turbo `large-v3` (better on Dutch) remains an open lever — see
[pipeline.md §2](pipeline.md).

**5.4 Diarization.** pyannote 3.1, exclusive (non-overlapping) turns → relative
`SPEAKER_nn` labels. Mapping those to named people is a separate step (5.5).

**5.5 Speaker ID.** Each turn is embedded (`pyannote/embedding`) and cosine-
matched to enrolled voiceprints; above threshold → name, else unknown. Enrolment
is additive — labelling a correction enrols that voice, and the archive
re-attributes. Far-field household audio matches at only ~0.3 cosine, so the
displayed confidence is a softmax "this person vs the others," not raw cosine.

**5.6 Storage (SQLite + FTS5)** — `recall.store` migrations:

- `sources(id, name, kind, port)` — each recorder (USB mic, a phone, uploads).
- `audio_segments(id, source_id, path, start_utc, end_utc, sample_rate,
  channels, transcribed_utc)` — the immutable raw segment files.
- `transcript_segments(id, audio_segment_id, start_utc, end_utc, text, language,
  asr_confidence, asr_model, speaker_label, speaker_id, speaker_guess,
  speaker_score, speaker_cluster, superseded_by, provenance, hidden_reason, …)` —
  the versioned turns; `superseded_by` + a `transcript_lineage` table express
  supersession (incl. N→1 merges).
- `speakers(id, name)` + `speaker_embeddings(speaker_id, vector,
  source_correction_id, source_segment_id, …)` — enrolled people and their
  reference voiceprints; a voiceprint can be seeded from a human correction or,
  since v16, taken directly from a confirmed transcript segment.
- `corrections(…, original_text, corrected_text, speaker, audio_confidence,
  hidden_reason)` — human ground truth: the fine-tune corpus and enrolment seed.
- `transcript_embeddings(segment_id, vector)` — each turn's cached voiceprint, so
  a guess re-matches against current voiceprints without re-embedding.
- `transcript_fts` — FTS5 over `text`. Audio referenced by path (immutable);
  transcripts are re-derivable, audio is the source of truth.

**5.7 Recall.** Angular + FastAPI web app on one origin (`:8000`): timeline,
full-text search with playback, review/correct queue, phone-as-mic recording, and
speaker labelling that enrols voices as they're confirmed (there is no separate
enrol screen — labelling *is* enrolment; §5.5). The recall layer proper: per-day
summaries (generated by the refine daemon for each finished day, `day_summaries`)
and **Ask** — FTS retrieval + a local LLM (mlx-lm) answering ONLY from retrieved
turns, cited; no evidence → an honest "the recordings don't show it", never an
improvised answer. The LLM weights are held by **one** process on the Mac
(`recall llm-host`, §5.8) rather than loaded into each consumer — 4 GB is worth
sharing, and the other consumer is a different project entirely (life's emotion
suggestions).

**5.8 One model, one holder.** Every process that wants generated text asks
`llm-host` over loopback (`recall.llm.make_generator`); it loads on demand,
serves one request at a time (one GPU), and releases the weights after five idle
minutes. Cross-machine work does not change shape: the fleet still cannot reach
this Mac (one-way WireGuard — see [`isis-migration.md`](isis-migration.md)), so
anything running there queues work the Mac pulls — the holder
sits behind that, not in front of it.

## 6. Continual improvement

Because raw audio is retained, every transcript is a **derived view**,
regenerable. Outputs are **versioned, never overwritten**: each carries its model
+ confidence and is marked `superseded_by` when a better pass replaces it
(live → worker → diarized refine → human correction). Re-derivation is a **full
recompute over whole segments**, never an incremental patch (reproducibility).
Low-confidence is surfaced for review and **weighted, never silently dropped.**
Corrections are training fuel — text into the ASR corpus, speaker into voiceprints.

## 7. Service & resilience

Separate launchd agents: **capture** (must never die) and the
**worker / live / refine** processes (killable, resume from unprocessed audio).
Health surfaces last-captured / last-transcribed timestamps, queue depth, disk
free. Explicit failure handling: disk full (keep capturing, stop transcribing),
mic unplug (alert + auto-resume), worker crash (resume from queue).

## 8. Privacy

100% local; no cloud ASR, no network in the hot path; encrypted at rest. The repo
holds **no transcript content and no personal identifiers** — names live only in
runtime enrolment data, never in the codebase or these docs.

## 9. Implementation

**Worker → Python** (Whisper, pyannote, PEFT — the ML ecosystem lives there;
latency isn't a requirement, so Python's speed is irrelevant). **Capturer →
sox | ffmpeg** today; an optional self-contained **Rust** daemon (exact
timestamps, custom ring buffer, tighter health) is a possible later hardening,
not needed yet. The two halves talk only through the filesystem + SQLite, so
neither depends on the other's runtime. Stack: SQLite+FTS5, Silero VAD,
pyannote 3.1, mlx-whisper, Nix devshell + uv venv, launchd services. Engineering
conventions (strict typing, TDD): [conventions.md](conventions.md).

## 10. Open questions

Retention (forever vs rolling window); audio scope (all vs speech-padded);
re-transcription cadence (scheduled vs on-demand). The recall/Q&A LLM is
Qwen2.5-7B-Instruct (4-bit, mlx-lm) — first pick, revisit as local models move.
