"""LoRA fine-tuning of Whisper on the corrections corpus.

Consumes a `manifest.jsonl` produced by recall.training.export_corpus and trains
small LoRA adapters on the household's corrected speech — adapting accent,
code-switching, vocabulary, and room acoustics (pipeline.md §2). After training,
point a transcriber at the adapter and re-transcribe to supersede old output.

This is the model-improvement half of the flywheel. It is **heavy** (torch +
transformers + peft + datasets, Apple-Silicon/GPU) and is lazily imported and
**not exercised by the test suite** — the same isolation as the pyannote and
mlx-whisper adapters. The export it consumes (recall.training) *is* tested.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

DEFAULT_BASE_MODEL = "openai/whisper-large-v3"

# Whisper's decoder is capped at 448 target tokens. A correction longer than that (a
# multi-sentence passage that's really several windows of speech, not one) can't be a
# single training label — including it crashes the trainer mid-run. Such clips are
# dropped from the corpus (and counted, never silently).
_MAX_LABEL_TOKENS = 448

# Below this, a train/eval split would leave a side empty — train on everything.
_MIN_FOR_EVAL_SPLIT = 2


@dataclass(frozen=True)
class FinetuneConfig:
    manifest: Path
    output_dir: Path
    base_model: str = DEFAULT_BASE_MODEL
    epochs: int = 3
    # 1e-4 is the agreed recipe (pipeline.md §5): the corpus sits near the overfit
    # boundary, so a gentler rate than the old 1e-3 plus early stopping on a
    # held-out slice is what keeps the adapter from memorising.
    learning_rate: float = 1e-4
    lora_rank: int = 16
    # Memory knobs, defaulted safe for unified memory (MPS). A full large-v3 forward at
    # batch 8 fp32 needs ~40 GB and OOMs a 42 GB machine, so keep the batch at 1 and
    # recover the effective size with gradient accumulation + activation checkpointing.
    batch_size: int = 1
    grad_accum: int = 8
    gradient_checkpointing: bool = True
    # Early stopping. With eval_holdout > 0 a deterministic slice is held out, eval
    # loss is measured each epoch, and training stops after `early_stopping_patience`
    # epochs without improvement — keeping the best checkpoint, not the last. 0
    # disables it (train the full `epochs`), preserving the old behaviour.
    eval_holdout: float = 0.0
    early_stopping_patience: int = 2


def _load_examples(manifest: Path) -> list[dict[str, Any]]:
    with manifest.open(encoding="utf-8") as handle:
        return [json.loads(line) for line in handle if line.strip()]


def select_device() -> str:
    """Prefer Apple-Silicon MPS, else CPU. (No CUDA on this hardware.)"""
    import torch  # noqa: PLC0415

    return "mps" if torch.backends.mps.is_available() else "cpu"


def transcribe_clips(
    records: list[dict[str, Any]],
    *,
    base_model: str = DEFAULT_BASE_MODEL,
    adapter_dir: Path | None = None,
    device: str | None = None,
) -> list[str]:
    """Transcribe each record's `audio` clip and return one hypothesis per clip.

    With `adapter_dir`, loads the LoRA on top of the base — this is how the pilot
    compares base vs adapter on the same clips. Language is forced from each
    record's `language` when present (isolating acoustic/vocab adaptation from
    language detection). Heavy: transformers/peft, lazily imported. Decodes audio
    via ffmpeg and runs in fp32 — the same torchcodec/MPS-fp16 avoidance as
    training.
    """
    import torch  # noqa: PLC0415
    from transformers import (  # noqa: PLC0415
        WhisperForConditionalGeneration,
        WhisperProcessor,
    )

    from recall.asr import decode_pcm_f32  # noqa: PLC0415

    device = device or select_device()
    processor = WhisperProcessor.from_pretrained(base_model)
    base = WhisperForConditionalGeneration.from_pretrained(
        base_model, dtype=torch.float32
    )
    # Union of WhisperForConditionalGeneration and PeftModel — keep it loose so
    # the optional LoRA wrap below doesn't fight the static type.
    model: Any = base
    if adapter_dir is not None:
        from peft import PeftModel  # noqa: PLC0415

        model = PeftModel.from_pretrained(base, str(adapter_dir))
    model = model.to(device)
    model.eval()

    hyps: list[str] = []
    for record in records:
        array = decode_pcm_f32(Path(record["audio"]), sample_rate=16000)
        feats = processor.feature_extractor(
            array, sampling_rate=16000, return_tensors="pt"
        ).input_features.to(device)
        kwargs: dict[str, Any] = {"task": "transcribe"}
        if record.get("language"):
            kwargs["language"] = record["language"]
        with torch.no_grad():
            ids = model.generate(feats, **kwargs)
        text = processor.tokenizer.batch_decode(ids, skip_special_tokens=True)[0]
        hyps.append(text.strip())
    return hyps


def finetune_lora(config: FinetuneConfig) -> Path:
    """Train LoRA adapters from the corpus; return the adapter directory.

    Requires the training environment (torch/transformers/peft/datasets). Follows
    the canonical HuggingFace Whisper + PEFT recipe; tune in one place here.
    """
    # Lazy, heavy imports — only when actually training.
    import torch  # noqa: PLC0415
    from datasets import Dataset  # noqa: PLC0415
    from peft import LoraConfig, get_peft_model  # noqa: PLC0415
    from transformers import (  # noqa: PLC0415
        EarlyStoppingCallback,
        Seq2SeqTrainer,
        Seq2SeqTrainingArguments,
        TrainerCallback,
        WhisperForConditionalGeneration,
        WhisperProcessor,
    )

    from recall.asr import decode_pcm_f32  # noqa: PLC0415

    processor = WhisperProcessor.from_pretrained(config.base_model)
    examples = _load_examples(config.manifest)
    # Keep `audio` as the clip path and decode it ourselves (ffmpeg) in `prepare`,
    # rather than letting datasets' Audio() decode via torchcodec — that backend
    # can't load its shared libs on this torch stack.
    dataset = Dataset.from_list(examples)

    def prepare(batch: dict[str, Any]) -> dict[str, Any]:
        array = decode_pcm_f32(Path(batch["audio"]), sample_rate=16000)
        batch["input_features"] = processor.feature_extractor(
            array, sampling_rate=16000
        ).input_features[0]
        # Stamp each label with THIS example's language + the transcribe task, so the
        # decoder prefix (`<|sot|><|nl|><|transcribe|>...`) matches the speech. Without
        # it the tokenizer emits a bare `<|sot|><|notimestamps|>` prefix - no language
        # token at all - and a bilingual corpus corrupts Whisper's language head (the
        # 2026-07-08 adapter decoded Dutch as Cyrillic/Hebrew; see pipeline.md §5).
        processor.tokenizer.set_prefix_tokens(
            language=batch.get("language") or "en", task="transcribe"
        )
        batch["labels"] = processor.tokenizer(batch["text"]).input_ids
        return batch

    dataset = dataset.map(prepare, remove_columns=dataset.column_names)

    # Drop clips whose label exceeds Whisper's decoder window — they'd crash training.
    before = len(dataset)
    dataset = dataset.filter(lambda b: len(b["labels"]) <= _MAX_LABEL_TOKENS)
    dropped = before - len(dataset)
    if dropped:
        print(
            f"[finetune] dropped {dropped} of {before} clips whose label exceeds "
            f"{_MAX_LABEL_TOKENS} tokens (too long for one Whisper window)"
        )

    # Hold out a deterministic slice for early stopping when asked. Too few
    # examples to split: train on everything (no eval, no early stop).
    eval_dataset = None
    if config.eval_holdout > 0.0 and len(dataset) >= _MIN_FOR_EVAL_SPLIT:
        split = dataset.train_test_split(test_size=config.eval_holdout, seed=0)
        dataset, eval_dataset = split["train"], split["test"]
        print(
            f"[finetune] {len(dataset)} train / {len(eval_dataset)} eval; early "
            f"stopping after {config.early_stopping_patience} epochs without gain"
        )

    # fp32 weights: the checkpoint ships fp16, but features are fp32 and MPS fp16
    # conv is unreliable — train the whole graph in float32.
    base = WhisperForConditionalGeneration.from_pretrained(
        config.base_model, dtype=torch.float32
    )
    if config.gradient_checkpointing:
        # Checkpointing is incompatible with the generation cache; trade compute
        # for the activation memory that otherwise OOMs unified memory.
        base.config.use_cache = False
    model = get_peft_model(
        base,
        LoraConfig(
            r=config.lora_rank,
            lora_alpha=config.lora_rank * 2,
            target_modules=["q_proj", "v_proj"],
            lora_dropout=0.05,
        ),
    )
    if config.gradient_checkpointing:
        # With a frozen base, inputs must explicitly require grad for the
        # checkpointed graph to backprop into the LoRA adapters.
        model.enable_input_require_grads()

    def collate(features: list[dict[str, Any]]) -> dict[str, Any]:
        inputs: dict[str, Any] = processor.feature_extractor.pad(
            [{"input_features": f["input_features"]} for f in features],
            return_tensors="pt",
        )
        labels = processor.tokenizer.pad(
            [{"input_ids": f["labels"]} for f in features], return_tensors="pt"
        )
        label_ids = labels["input_ids"].masked_fill(labels.attention_mask.ne(1), -100)
        # The model prepends the decoder-start token (`<|sot|>`) itself when it shifts
        # labels right, so drop the leading `<|sot|>` the tokenizer added - otherwise
        # it's duplicated and the trained prefix no longer lines up with the
        # forced-language decoding used at inference (the canonical HF recipe's last
        # step, needed now that labels carry the full language/task prefix).
        sot = processor.tokenizer.convert_tokens_to_ids("<|startoftranscript|>")
        if (label_ids[:, 0] == sot).all():
            label_ids = label_ids[:, 1:]
        inputs["labels"] = label_ids
        return inputs

    # Evaluate + checkpoint per epoch only when there's a held-out slice; keep the
    # best (lowest eval-loss) model and stop early once it plateaus.
    early_stop = eval_dataset is not None
    args = Seq2SeqTrainingArguments(
        output_dir=str(config.output_dir),
        num_train_epochs=config.epochs,
        learning_rate=config.learning_rate,
        per_device_train_batch_size=config.batch_size,
        gradient_accumulation_steps=config.grad_accum,
        gradient_checkpointing=config.gradient_checkpointing,
        gradient_checkpointing_kwargs={"use_reentrant": False},
        remove_unused_columns=False,
        eval_strategy="epoch" if early_stop else "no",
        # HF defaults eval batch to 8 regardless of the train batch — on fp32
        # large-v3 that's 8x the forward memory and OOM-kills the eval pass, so
        # match it to the (deliberately tiny) train batch and stream logits off
        # the accelerator rather than piling a whole eval set into memory.
        per_device_eval_batch_size=config.batch_size,
        eval_accumulation_steps=1,
        save_strategy="epoch" if early_stop else "no",
        load_best_model_at_end=early_stop,
        metric_for_best_model="eval_loss",
        greater_is_better=False,
    )
    callbacks: list[TrainerCallback] = (
        [EarlyStoppingCallback(early_stopping_patience=config.early_stopping_patience)]
        if early_stop
        else []
    )
    trainer = Seq2SeqTrainer(
        model=model,
        args=args,
        train_dataset=dataset,
        eval_dataset=eval_dataset,
        data_collator=collate,
        callbacks=callbacks,
    )
    trainer.train()

    adapter_dir = config.output_dir / "adapter"
    model.save_pretrained(str(adapter_dir))
    return adapter_dir
