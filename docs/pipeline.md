# Analysis pipeline: transcription, language, speakers, improvement

The worker side: turning retained raw audio into attributed, searchable
transcripts — and how the system starts imperfect and improves without ever
committing to a result.

## 1. The trainable stack

No single "trainable algorithm" — a stack of models, of which only two benefit
from training on *this household's people*, both fed by the same growing pool of
labelled corrections.

| Component | Model | "Training" | Person-specific |
|---|---|---|---|
| ASR | Whisper large-v3-turbo | LoRA fine-tune on corrected audio | **yes** — accent, vocab, code-switch |
| Language ID | Whisper built-in | priors, rarely retrained | indirectly |
| Diarization | pyannote 3.1 | off-the-shelf | no |
| Speaker ID | `pyannote/embedding` | **enrolment** (reference voiceprints) | **yes** — the core |
| VAD | Silero | off-the-shelf | no |

The flywheel: every corrected transcript and confirmed speaker becomes a labelled
example that improves ASR fine-tuning and speaker enrolment.

## 2. Bilingual EN + Dutch, and the model choice

One multilingual model, not two — Whisper handles EN and NL natively. **What runs
today: `large-v3-turbo`** (`asr.DEFAULT_MODEL`), every pass. Plain `large-v3` is
more accurate on Dutch and the M4 has the headroom (latency isn't a requirement),
so moving the accuracy passes (refine/reprocess) to non-turbo `large-v3` is an
**open, un-taken lever** — not something the code already does. The hard case is
**code-switching** (EN/NL mixed in one sentence): vanilla Whisper picks one
language per window and mangles the other — the main thing household fine-tuning
fixes once code-switched examples accumulate.

## 3. Language decision

**Today:** the whole segment is transcribed once and Whisper's built-in
**per-segment** language detection is taken as-is — *not* per speaker-turn.

**Designed, not built:** decide per turn from three signals — per-utterance LID,
dual-decode confidence (transcribe forced-EN and forced-NL, keep the higher
log-prob), and a per-speaker language prior (once the speaker is known). Signal 3
needs speaker ID, so language and speaker recognition would form one loop.

## 4. Recognising the human

Diarize (pyannote) → embed each turn → cosine-match to enrolled voiceprints;
above threshold → that person, below → **unknown**. Unknowns cluster over time
and a recurring one is surfaced for labelling ("who is this voice?"); once
labelled it's enrolled, so the roster bootstraps and visitors are added without a
ceremony. Each confirmed attribution adds a sample → sharper profile →
re-attribute history. Caveats (hence "wrong at first, better over time"):
overlapping speech and far-field degrade diarization and embeddings; voices
drift; the match threshold is a real tuning knob.

**Bootstrapping enrolment**, two ways used together: *cluster-then-label*
(preferred, low-friction — name the clusters diarization already found) and an
optional *enrolment session* (read ~1–2 min for a clean profile) for the primary
members.

### Measuring attribution

`recall score-attribution <id>` replays a corrected recording back through
diarize + word-assignment and reports the fraction of words given the right speaker,
swept over the smoothing threshold (`align._MIN_TURN_S`) and broken down by where the
errors fall (near a speaker change, interior of a turn, inside short turns, and per
speaker). It's how the alignment knobs get tuned on real human ground truth instead of
guesses. On the densest ground-truth set so far it read **~94%** per-word, with the
errors concentrated **at speaker changes (~76%) and in short interjections (~37%)**,
while turn interiors were ~98%. Two heuristic levers were measured and *ruled out* —
the smoothing threshold barely moves it, and switching word→turn assignment from
midpoint to maximum-overlap changed nothing — so the remaining errors are input-precision
(pyannote boundaries / word timings), not the assignment rule. The harness gates any
future attribution change: prove the delta, don't eyeball it.

## 5. Continual improvement

Raw audio is retained, so every transcript/attribution is a **derived view**,
regenerable. Never commit: each output stores its model + confidence and is
`superseded_by` when a better pass replaces it. Re-derive on improvement as a
**full recompute over whole segments**, never an incremental patch. Confidence
everywhere — low-confidence is surfaced for review and **weighted, never
dropped.**

### The implemented loop

1. **Collect** — the review UI surfaces the lowest-confidence transcripts; each
   fix supersedes the turn and records a labelled pair in `corrections`.
2. **Export** — `recall export-training` slices each correction's audio and writes
   `clips/` + `manifest.jsonl` of `{audio, text, language}`.
3. **Fine-tune** — `recall finetune` LoRA-fine-tunes Whisper (HF + PEFT; heavy,
   isolated, not run by tests).
4. **Evaluate (the gate)** — `recall finetune-pilot` splits the corpus, trains on
   part, and reports base-vs-adapter WER on a **held-out** set the adapter never
   saw. Only the held-out number (and the *delta*) proves a real gain — and since
   each pilot draws its own held-out split, absolute WER isn't comparable across
   runs, only the sign of the delta. Measured: a small corpus overfits and *loses*
   (~71 pairs, 16.8% → 19.2%); past ~150 pairs the adapter wins its own held-out set
   (12.8% → 9.4%), and the most recent run (~400 pairs) won 42.2% → 36.4%. Numbers
   are isolated-clip, so they run higher than production.
5. **Spot-check on real audio (A/B)** — the pilot proves a gain on held-out
   *training clips*; an A/B comparison proves it on a real past recording,
   **non-destructively**. It runs the old model and the new adapter over the
   recording — or a window of it — and reports a per-segment text diff (what
   changed) plus, wherever you have corrections in that range, each model's WER
   against your ground truth, with a verdict. Nothing is superseded — the safe
   look before committing to a re-derive. (No corrections in range → text diff
   only, no WER.) Two front doors:
   - **CLI** — `recall ab-compare --source <rec> --model-b <adapter>
     --base-model <hf>` (`--model-a` defaults to the live stock model;
     `--from`/`--to` for a window) writes `ab-compare-<rec>.md` (+ `.json`).
   - **Web UI** — the **Compare** screen queues a run (defaulting to the stock
     model vs the latest trained adapter, so it's one field), the idle refine
     daemon runs it
     (operator-chosen + read-only, so it runs regardless of the pause and needs no
     HF_TOKEN), and the result view sorts the corrected spans by biggest A↔B
     disagreement, plays the audio of each, and highlights where each model
     deviates from your correction — the evidence for *why* one is better.
6. **Re-transcribe** — re-run ASR over the archive and supersede the old machine
   turns; human corrections are never touched. **Granularity matters:** Whisper needs
   ~30 s of context, so re-derive *whole segments*, not tiny re-sliced clips that
   hallucinate on a bare 2 s span. The whole-segment passes are `recall redrive`
   (VAD-gated, full context) and `recall refine` (diarized, word-aligned) — prefer
   these. `recall reprocess` is the narrower legacy path that re-transcribes individual
   turn clips for an improved model and won't degrade to a lower-confidence result; it's
   subject to that same context caveat, so it's only apt for re-running a confident,
   substantial turn through a new adapter.

**Deploying a winning adapter** runs it on these accuracy passes via an HF/PEFT
transcriber (`recall.hf_asr`), *not* the live path: pass the LoRA adapter directory as
`--model` (with `--base-model`, the HF base it was trained on) to `reprocess`, `redrive`,
or `refine`. The pass auto-detects an adapter dir (an `adapter_config.json`) and loads
base + adapter once; otherwise it stays on mlx-whisper. This is the right home for the
adapter: it's a non-turbo large-v3 LoRA, while the live worker runs large-v3-turbo for
latency — so the adapter belongs on the idle-gated re-derivation, never live capture.
`refine` gets per-word timings + probabilities from the HF path too (token timestamps +
transition scores), so it aligns and scores like the mlx path; its first-word start can
be a touch coarser than mlx (a known Whisper word-timestamp quirk), which doesn't affect
whole-segment speaker assignment.

### The next retrain (recipe agreed 2026-07-03, not yet run)

The 2026-06 adapter won its pilot but **regressed on real audio** (truncated long
one-shots — it learned early EOS from a corpus that was ~74% short clips), so it
was un-deployed. The corpus hygiene that fixes the cause has since shipped (the
train queue and export skip <2 s / <4-word backchannels and dedupe overlapping
re-corrections). The agreed parameters for the next `recall finetune` run:

- **Stitch adjacent clips into ≤30 s training windows** — Whisper is trained on
  ~30 s context; isolated 2–5 s clips are what taught the early EOS. Same rule
  as inference (§ re-transcribe: whole segments, never tiny clips).
- **Learning rate 1e-4** with **early stopping** on the held-out split, rather
  than a fixed epoch count — the pilot history shows this corpus size sits near
  the overfit boundary.
- **Gate = whole-segment A/B on real recordings** (`recall ab-compare`), not the
  pilot's held-out clip WER — the pilot already passed once while the adapter
  regressed in production shape. Only an A/B win re-points
  `scripts/recall-refine.sh` at `adapter-current`.
- **Run in a capture-idle window only** — two Whispers starve capture (sox
  buffer overrun = dropped samples), same constraint as refine.

> **Still a follow-up:** per-person adapters (selected at transcription time by the
> identified speaker), and an mlx conversion if the adapter ever needs the live path.
