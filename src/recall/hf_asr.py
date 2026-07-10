"""HF + PEFT transcriber — how a winning household adapter reaches the archive.

The live worker runs mlx-whisper (turbo) for latency; a household LoRA fine-tune is
HF/PEFT on non-turbo large-v3. So a proven adapter is deployed *here*, on the
idle-gated re-derivation passes, never the latency-critical live path.

`words=True` adds per-word timings + probabilities (from token timestamps and
transition scores), so the diarized whole-segment passes (refine/redrive) can align
and score just like the mlx path. Heavy (torch/transformers/peft, lazily imported);
the adapter-dir check, the long-audio windowing, and the token-to-word grouping are
unit-tested.
"""

from __future__ import annotations

import math
from collections.abc import Callable, Sequence
from dataclasses import replace
from pathlib import Path
from typing import Any

from recall.asr import AsrResult, AsrSegment, Transcriber, Word
from recall.finetune import DEFAULT_BASE_MODEL  # the base the adapter was trained on

_LANG_CODE_LEN = 2  # a Whisper language token is <|xx|>

# A Whisper encoder consumes a fixed 30 s mel window, and the HF feature extractor pads
# *or truncates* every input to it. So a clip longer than one window must be transcribed
# window by window and stitched back with each window's offset — otherwise everything
# past the first 30 s is silently dropped (a 34-min meeting came back as one 30-s turn).
# The mlx worker path gets this windowing free from mlx-whisper; the HF adapter path
# does it explicitly, below.
_WINDOW_S = 30.0


def plan_windows(
    n_samples: int, sample_rate: int, window_s: float = _WINDOW_S
) -> list[tuple[int, int]]:
    """Contiguous, gap-free ``[start, end)`` sample ranges of at most `window_s`
    covering all of `n_samples` — the fixed windows a Whisper encoder takes. Every
    sample lands in exactly one window; an empty clip yields no windows."""
    step = max(1, round(window_s * sample_rate))
    return [(s, min(s + step, n_samples)) for s in range(0, max(0, n_samples), step)]


def merge_windows(parts: Sequence[tuple[float, AsrResult]]) -> AsrResult:
    """Stitch per-window results back into one clip-relative result. `parts` is
    ``(offset_s, window_result)`` in clip order; each window is timed from its own
    start, so every segment and word time is shifted by that window's offset. Language
    is the first window's (it carries the most speech context)."""
    if not parts:
        return AsrResult(language="en", language_confidence=None, segments=())
    segments: list[AsrSegment] = []
    for offset, res in parts:
        for seg in res.segments:
            segments.append(
                replace(
                    seg,
                    start=seg.start + offset,
                    end=seg.end + offset,
                    words=tuple(
                        replace(w, start=w.start + offset, end=w.end + offset)
                        for w in seg.words
                    ),
                )
            )
    first = parts[0][1]
    return AsrResult(
        language=first.language,
        language_confidence=first.language_confidence,
        segments=tuple(segments),
    )


def transcribe_windowed(
    array: Any,  # noqa: ANN401 - np.ndarray, kept off the module import surface
    sample_rate: int,
    window: Callable[[Any], AsrResult],
    *,
    window_s: float = _WINDOW_S,
) -> AsrResult:
    """Transcribe a whole waveform by feeding each fixed `window_s` slice to `window`
    and stitching the pieces back with their offsets. This is what stops a clip longer
    than one Whisper window from being truncated to its first 30 s. Pure given `window`,
    so the windowing is unit-tested without loading a model."""
    parts = [
        (start / sample_rate, window(array[start:end]))
        for start, end in plan_windows(len(array), sample_rate, window_s)
    ]
    return merge_windows(parts)


def is_adapter_dir(model: str) -> bool:
    """True if `model` points at a local LoRA adapter directory (a PEFT
    `adapter_config.json`), as opposed to an mlx-whisper model id or path. This is how
    the accuracy passes decide between the HF adapter and the plain mlx model."""
    path = Path(model)
    return path.is_dir() and (path / "adapter_config.json").is_file()


def _group_words(
    tokens: Sequence[tuple[str, float, float]], clip_end: float
) -> tuple[Word, ...]:
    """Group Whisper sub-word tokens into words. `tokens` is (text, start, log_prob) per
    content token in order; a new word starts at a leading space (Whisper's word
    boundary). A word's end is the next word's start (last word's is `clip_end`); its
    probability is exp(mean token log-prob), clamped. Token text keeps its leading
    space, matching the mlx path so the downstream join is identical."""
    groups: list[list[tuple[str, float, float]]] = []
    for tok in tokens:
        if not groups or (tok[0].startswith(" ") and groups):
            groups.append([tok])
        else:
            groups[-1].append(tok)

    words: list[Word] = []
    for i, group in enumerate(groups):
        text = "".join(t[0] for t in group)
        if not text.strip():
            continue
        start = group[0][1]
        end = groups[i + 1][0][1] if i + 1 < len(groups) else clip_end
        mean_lp = sum(t[2] for t in group) / len(group)
        words.append(
            Word(
                start=start,
                end=max(end, start),
                text=text,
                probability=min(1.0, max(0.0, math.exp(mean_lp))),
            )
        )
    return tuple(words)


def _language_of(sequence: Any, tokenizer: Any) -> str:  # noqa: ANN401 - torch/HF objects
    """The language Whisper predicted — the first `<|xx|>` token in the sequence (it
    sits right after `<|startoftranscript|>`). Best-effort metadata; defaults to en."""
    try:
        for token in sequence.tolist():
            text = tokenizer.decode([token])
            if text.startswith("<|") and text.endswith("|>"):
                code = text[2:-2]
                if len(code) == _LANG_CODE_LEN and code.isalpha():
                    return str(code)
    except Exception:  # best-effort metadata, never fatal
        return "en"
    return "en"


def _field(out: Any, name: str) -> Any:  # noqa: ANN401 - HF generate output
    """Whisper's generate returns a ModelOutput normally but a plain dict when
    return_token_timestamps is set — read either."""
    return out[name] if isinstance(out, dict) else getattr(out, name)


def _content_tokens(
    out: Any,  # noqa: ANN401 - HF generate output
    scores: Any,  # noqa: ANN401 - per-generated-token log-probs
    tokenizer: Any,  # noqa: ANN401 - HF tokenizer
) -> list[tuple[str, float, float]]:
    """(text, start_time, log_prob) per non-special generated token, for `_group_words`.
    Token timestamps may or may not include the decoder prompt; align by length."""
    seq = _field(out, "sequences")[0]
    times = _field(out, "token_timestamps")[0]
    prompt = len(seq) - len(scores)  # generated tokens are seq[prompt:]
    ts_offset = len(seq) - len(times)  # times may exclude the prompt too
    content: list[tuple[str, float, float]] = []
    for i in range(prompt, len(seq)):
        text = tokenizer.decode([int(seq[i])])
        if text.startswith("<|") and text.endswith("|>"):
            continue  # special / timestamp token, not a word
        ts_i = i - ts_offset
        start = float(times[ts_i]) if 0 <= ts_i < len(times) else 0.0
        content.append((text, start, float(scores[i - prompt])))
    return content


def make_hf_transcriber(
    adapter_dir: str,
    *,
    base_model: str = DEFAULT_BASE_MODEL,
    words: bool = False,
    device: str | None = None,
) -> Transcriber:
    """Load `base_model` + the LoRA at `adapter_dir` once and return a transcriber
    `(audio) -> AsrResult`. `words=True` adds per-word timings + probabilities (for the
    diarized refine/redrive passes). Reuses the same fp32 + ffmpeg-decode path as
    training, so production matches what the adapter saw."""
    import torch  # noqa: PLC0415 - heavy, optional
    from peft import PeftModel  # noqa: PLC0415
    from transformers import (  # noqa: PLC0415
        WhisperForConditionalGeneration,
        WhisperProcessor,
    )

    from recall.asr import decode_pcm_f32  # noqa: PLC0415
    from recall.finetune import select_device  # noqa: PLC0415

    device = device or select_device()
    processor = WhisperProcessor.from_pretrained(base_model)
    base = WhisperForConditionalGeneration.from_pretrained(
        base_model, dtype=torch.float32
    )
    model: Any = PeftModel.from_pretrained(base, adapter_dir).to(device)
    model.eval()
    gen: Any = base  # compute_transition_scores: transformers' stub self-type is off

    kwargs: dict[str, Any] = {
        "task": "transcribe",
        "return_dict_in_generate": True,
        "output_scores": True,
        # Anti-hallucination decoding, matching the mlx path: re-decode a window at a
        # higher temperature when it trips the compression-ratio (repetition) or logprob
        # (gibberish) guard, and forbid repeated 6-grams so a long clip can't spin into
        # a loop (plain greedy generate did — seen on a 36s turn).
        "temperature": (0.0, 0.2, 0.4, 0.6, 0.8, 1.0),
        "compression_ratio_threshold": 2.4,
        "logprob_threshold": -1.0,
        "no_speech_threshold": 0.6,
        "no_repeat_ngram_size": 6,
    }
    if words:
        kwargs["return_token_timestamps"] = True

    def transcribe_window(chunk: Any) -> AsrResult:  # noqa: ANN401 - np.ndarray slice
        """Transcribe one ≤30 s window (clip-relative from 0). The whole-clip windowing
        is `transcribe_windowed`; this is the single-window model call it drives."""
        feats = processor.feature_extractor(
            chunk, sampling_rate=16000, return_tensors="pt"
        ).input_features.to(device)
        with torch.no_grad():
            out = model.generate(feats, **kwargs)
        seq = _field(out, "sequences")[0]
        text = processor.tokenizer.decode(seq, skip_special_tokens=True).strip()
        # Confidence proxy: mean per-token log-prob → exp, the same basis as mlx's path.
        scores = gen.compute_transition_scores(
            _field(out, "sequences"), _field(out, "scores"), normalize_logits=True
        )[0]
        finite = scores[torch.isfinite(scores)]
        avg_logprob = float(finite.mean()) if finite.numel() else -1.0
        clip_end = len(chunk) / 16000.0
        word_tuple = (
            _group_words(_content_tokens(out, scores, processor.tokenizer), clip_end)
            if words
            else ()
        )
        segment = AsrSegment(
            start=0.0,
            end=clip_end,
            text=text,
            avg_logprob=avg_logprob,
            no_speech_prob=0.0,
            words=word_tuple,
        )
        return AsrResult(
            language=_language_of(seq, processor.tokenizer),
            language_confidence=None,
            segments=(segment,),
        )

    def transcribe(audio: Path) -> AsrResult:
        array = decode_pcm_f32(audio, sample_rate=16000)
        return transcribe_windowed(array, 16000, transcribe_window)

    return transcribe
