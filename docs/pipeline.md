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
guesses. `--max-segments` bounds a run: a whole-archive source replays for hours and only
reports at the end.

Measured over seven corrected recordings (2026-07-29), the headline is **98%+ per-word**
almost everywhere — and that number hides the whole problem. Errors live at speaker
changes; turn interiors run 98.5–99.6%. **The household capture is the worst case and the
best case at once:** 98.7% overall, 99.6% in interiors, but **73.8% near a speaker
change** — the lowest measured. Only ~4% of its words sit near a change, so a quarter of
them being wrong barely dents the headline while being audible at every single handover.
Judge attribution work on the near-change column, never on the total.

**What makes a recording bad is not yet established.** Seven recordings is too few to
separate the candidates, and the obvious one does not survive contact. Speaker count
looks decisive until you count it per *diarized unit* rather than per source — the
diarizer sees one audio segment at a time, and the household's 60 s segments average
**1.15** distinct speakers (145 of 170 hold only one). Against near-change accuracy:

| near-change | words | speakers/segment | segment length | mean turn |
|---|---|---|---|---|
| 73.8% | 103 | 1.15 | 60 s | 3.9 s |
| 76.0% | 575 | 3 | 18.5 min | 15.8 s |
| 87.7% | 865 | 2 | 33.8 min | 14.4 s |
| 88.9% | 27 | 1.25 | 60 s | 5.3 s |
| 98.5% | 324 | 2 | 13.4 min | 10.8 s |
| 99.8% | 430 | 2 | 20.4 min | 11.4 s |
| 100.0% | 358 | 2 | 33.5 min | 27.7 s |

Neither speaker count nor turn length orders that column: 1.15 speakers scores worst and
2 scores perfect; the best recording has the longest turns and the second-worst has the
second-longest.

**The diarization window does — confirmed by control (`--chop`).** Take the two
recordings that score ~100% at handovers as single multi-minute files, and re-score them
cut into independent 60 s pieces: the shape live capture gives a conversation, where each
segment is diarized alone and no cluster identity survives a file boundary.

| recording | window | near a change | short turns | interior | overall |
|---|---|---|---|---|---|
| D | whole (33.5 min) | 100.0% | 100.0% | 100.0% | 100.0% |
| D | chopped to 60 s | **81.1%** | **50.8%** | 99.4% | 98.2% |
| E | whole (20.4 min) | 99.8% | 100.0% | 100.0% | 100.0% |
| E | chopped to 60 s | **90.9%** | **74.3%** | 98.4% | 97.1% |

Same audio, same truth, same code — only the window. Handovers lose up to 18.9 points and
short interjections up to 49, while interiors barely move: the signature of the complaint.
The mechanism is that a 60 s clip gives clustering an order of magnitude fewer embeddings
(against `min_cluster_size: 12` on 10 s windows) and cluster identity cannot cross a
segment boundary.

**It's a dose-response, and it says how long the window has to be.** Recording D at four
window lengths:

| window | near a change | short turns | interior | wall time |
|---|---|---|---|---|
| 60 s | 81.1% | 50.8% | 99.4% | 36m51s |
| 180 s | 92.0% | 84.2% | 99.5% | 42m09s |
| 600 s | 98.0% | 96.7% | 99.8% | 37m34s |
| whole (2010 s) | 100.0% | 100.0% | 100.0% | 37m02s |

Monotonic in window length and **flat in wall time** — the replay costs the same however
it's cut, so window length is free. Ten minutes recovers 16.9 of the 18.9 lost points
(~90%) and three minutes recovers 58%, so a fix does not need the whole recording: a
bounded multi-minute window gets nearly all of it, which matters because `refine` has to
hold the window in memory and re-derive whole segments.

**The fix is time-neutral**, which is what made it look worth doing: recording D took
37m02s whole and 36m51s chopped. A longer diarization window costs no more wall clock —
the per-call overhead and the longer attention window cancel out. `refine` diarizes one
audio segment at a time (60 s for live capture), so diarizing *runs of consecutive
segments* as one window was the indicated change, and it is idle-gated so it competes with
nothing. **Read that with the household result below — on live capture the same lever
made attribution worse, and the change was not made.**

Not the whole story: chopped meetings sit at 81.1% and 90.9% while the household sits at
73.8%, so the window accounts for most of the gap and something else — far-field room
audio, genuinely simultaneous family speech — accounts for the remaining 7–17 points.

**The mirror experiment on household audio FAILED — the fix does not transfer.** Ninety
consecutive household segments, each re-scored inside a window of its temporally adjacent
neighbours (`--context`, which stops at any recording gap so a join never invents
adjacency):

| window | near a change | short turns | interior | overall | words scored | wall |
|---|---|---|---|---|---|---|
| 1 segment (60 s) | 77.3% | 88.1% | 99.5% | 98.8% | 2023 | 1h57m |
| ±1 segment (~180 s) | 78.3% | 87.2% | 99.0% | 98.5% | 2460 | 4h16m |
| ±2 segments (~300 s) | **65.6%** | 90.2% | 98.6% | 97.6% | 2115 | 7h43m |

Nothing like the meeting curve, which gained 10.9 points at the same first step. The
±1 arm moves by less than one word; the ±2 arm is 11.7 points *worse*, and interiors —
which a window change should barely touch — fall monotonically across all three.

Two things limit how hard this can be read, both worth stating rather than filing the
result as clean. **The arms do not share a word set** (2023 / 2460 / 2115 words): a longer
window changes what Whisper emits for the same centre segment, so these are unpaired
comparisons and a one-point difference means nothing. Only the ±2 collapse is larger than
that noise. And **wall time is not flat here**, unlike the `--chop` experiment: the eval
re-transcribes an overlapping window per segment, so its cost scales with the window and
says nothing about what a production change would cost, which would diarize each run once.

Inference, not measurement: the household's segments average 1.15 speakers, so extending
the window adds voices and far-field room noise that a 60 s clip did not contain, giving
clustering more to get wrong — where a meeting's extra context is the same two or three
voices recurring. That would explain why the same lever helps one and hurts the other.

So the indicated change above stays **unbuilt**. The near-change gap on household capture
is real and unexplained; the window is not its cause.

Read the household figure with its sample in mind: 103 near-change words, and since a
near-change word needs two differently-labelled truth spans in the same segment, they come
from roughly the 25 segments that have more than one speaker.

Three heuristic levers were measured and *ruled out* —
the smoothing threshold barely moves it, switching word→turn assignment from
midpoint to maximum-overlap changed nothing, and using pyannote's **overlap-aware**
diarization instead of its exclusive one (below) lost — so the remaining errors are
input-precision (pyannote boundaries / word timings), not the assignment rule. The
harness gates any future attribution change: prove the delta, don't eyeball it.

**Ruled out: the overlap-aware view (measured 2026-07-29, then removed).** pyannote
returns two diarizations. The *exclusive* one — what `recall.diarize` keeps — resolves
simultaneous speech to a single voice, normally the one already talking, so its boundary
sits late and the incoming speaker's first words join the previous turn: the error people
actually notice. Deciding each word by coverage on the *overlap-aware* view instead reads
like the obvious fix. It is not. Scored over six corrected recordings, 18 368 words:

Recordings are unlabelled on purpose — a meeting id carries its date, and some of those
dates are denylisted (`check-pii`). Re-derive the table by running `score-attribution`
over every source that has human speaker labels; the point is the shape, not which
meeting is which.

| recording | words | exclusive | overlap | ≤0.2s | ≤0.4s | ≤0.8s |
|---|---|---|---|---|---|---|
| A (meeting) | 2903 | 94.0% | **94.5%** | 94.0% | 94.0% | 94.0% |
| B (meeting) | 2117 | **99.7%** | 98.1% | 99.4% | 99.3% | 99.1% |
| C (meeting) | 4981 | 96.6% | 95.8% | 96.6% | **96.7%** | 96.4% |
| D (meeting) | 5307 | **100.0%** | 98.8% | 99.9% | 99.9% | 99.8% |
| E (meeting) | 2565 | **100.0%** | 97.9% | 99.8% | 99.7% | 99.4% |
| F (phone mic) | 495 | 99.2% | **99.4%** | 99.2% | 99.4% | 99.4% |
| **all** | **18368** | **98.08%** | 97.11% | 98.00% | 97.99% | 97.81% |

Unbounded it over-corrects: the non-exclusive view extends *both* speakers across a
handover, so "covers more" keeps choosing the incoming speaker and a late boundary
becomes an early one (−4.7pt near a change on C, −11.2pt on E, which the exclusive rule
already gets 99.8% right). Capping the contested stretch (the ≤N columns)
stops the harm and yields nothing: every bound lands at or just below baseline. The code
was removed — see `git log` around 2026-07-29 for the implementation and its unit tests.

Two lessons the numbers carry, both costlier to relearn than to read:

- **One recording proves nothing.** A is the only one of six where the rule helps, and
  it is an outlier at both ends: worst baseline and sole win. Measured on it alone the
  change ships.
- **The error is a property of the recording, not the algorithm.** Near-change accuracy
  ranges from 76.0% (A) to 100.0% (D) with identical code. Four of six
  recordings are already ≥99.2% overall, so any future attribution work should start by
  asking *which recordings are bad and what they have in common* — mic, room, number of
  speakers — rather than by changing the assignment rule for all of them.

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

### The deployed adapter (recipe + language fix)

The 2026-06 adapter won its pilot but **regressed on real audio** (truncated long
one-shots — it learned early EOS from a corpus that was ~74% short clips), so it
was un-deployed. The fix is wired into the export + train path
(`recall.training` / `recall.finetune`); the parameters:

- **Stitch adjacent clips into ≤30 s training windows** — `export_corpus` merges
  adjacent same-speaker corrections within one audio segment (gap ≤ 0.4 s, capped
  at 30 s) into one window, so the label reads as continuous speech instead of the
  isolated 2–5 s clips that taught the early EOS. Two guards keep the audio honest:
  the small positive gap can't hide another voice or uncorrected speech, and both
  sides must be the same named speaker; overlapping spans (re-corrections of the
  same words) still fall through to dedup. A lone correction over 30 s is dropped —
  it can't be one clean window (Whisper truncates the audio but keeps the label).
- **Learning rate 1e-4** (the `finetune` default) with **early stopping** on a
  held-out slice (`--eval-holdout`, default 0.15; patience 2), keeping the best
  checkpoint — this corpus sits near the overfit boundary, so a gentler rate than
  the old 1e-3 plus early stopping is what stops it memorising.
- **Gate = whole-segment A/B on real recordings** (`recall ab-compare`), not the
  pilot's held-out clip WER — the pilot already passed once while the adapter
  regressed in production shape. Only an A/B win re-points the refine agent
  (`deploy/hm-agents.nix`) at `adapter-current`.
- **Run in a capture-idle window only** — two Whispers starve capture (sox
  buffer overrun = dropped samples), same constraint as refine.

**How it got here — two runs.** The first 2026-07-08 run trained cleanly on the
stitched corpus (275 windows, none over 30 s: eval loss 2.32 → 0.34, early-stopped,
no OOM, no truncation) but **failed the A/B gate**: on the 06-14 usb window
(74 corrections) it emitted **non-Latin script (Cyrillic/Hebrew) on 36 of 74**, e.g.
Dutch *"En jij doet niks fout."* → *"И ти не правиш нищо…"*. Its headline mean-WER
"win" (stock 1.12 vs adapter 0.53) was an artefact of one stock loop-hallucination
(WER 74) — drop it and stock's real WER was ~0.11, far ahead. So the gate held the
adapter back, catching what the pilot's held-out loss (0.34) hid — the same
pilot-vs-reality gap, a second time.

Root cause (confirmed in `finetune.py`): the label tokeniser added **no per-example
language prefix**, so a 156-nl / 118-en / 1-de corpus was labelled with a bare
`<|sot|><|notimestamps|>` (no language, no task token), corrupting Whisper's language
head. The fix stamps each label with its own language + the transcribe task
(`set_prefix_tokens(language=…, task="transcribe")`) and strips the duplicate leading
`<|sot|>` so training lines up with forced-language decoding. The **retrain
(adapter-20260708b)** then trained even cleaner (eval loss 0.80 → 0.267) and **won
the same A/B gate**: 0/74 garbling, mean WER 0.125 → 0.064, 18 per-correction wins to
6 trivial (single-word article/dialect) losses. It ran the idle refine pass from
2026-07-09, then was **reverted on 2026-07-11**: the win was only ever measured on
short clips, and on long recordings full fp32 large-v3 (a 32-layer decoder vs turbo's
4) is ~8x slower. Refine is back on turbo; the args that re-enable the adapter are kept
in `deploy/hm-agents.nix` next to the refine agent.

> **Still a follow-up:** per-person adapters (selected at transcription time by the
> identified speaker), and an mlx conversion if the adapter ever needs the live path.
