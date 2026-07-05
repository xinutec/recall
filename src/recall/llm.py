"""Local text generation for the recall layer (summaries, ask-the-archive).

Same shape as the ASR side: a `Generator` Protocol the pure logic is written
against, and one heavy factory (`make_mlx_generator`) that lazily imports mlx-lm
and loads a quantised instruct model on Metal — 100% local, matching the
design's no-cloud rule. Tests inject plain functions; only the factory touches ML.
"""

from __future__ import annotations

from typing import Protocol

# Qwen2.5 7B (4-bit): strong EN+NL instruction following, ~4.5 GB resident — fits
# alongside Whisper on the M4/32GB. Overridable per call site (--llm flags).
DEFAULT_LLM = "mlx-community/Qwen2.5-7B-Instruct-4bit"

# Summaries/answers are short; a bound keeps a runaway generation from pinning
# the GPU for minutes on a bad prompt.
_MAX_TOKENS = 600


class Generator(Protocol):
    """Anything that turns a prompt into generated text."""

    def __call__(self, prompt: str, /) -> str: ...


def make_mlx_generator(model: str = DEFAULT_LLM) -> Generator:
    """Load `model` once (lazy heavy import) and return a chat-templated generator."""
    from mlx_lm import generate, load  # noqa: PLC0415 - heavy ML import stays lazy

    # load()'s return type is a union (a 3-tuple only when return_config=True);
    # narrow the 2-tuple explicitly for mypy.
    loaded = load(model)
    llm, tokenizer = loaded[0], loaded[1]

    def run(prompt: str, /) -> str:
        # TokenizerWrapper delegates to the underlying HF tokenizer, which is
        # untyped — narrow the one boundary value instead of waiving the module.
        templated: str = tokenizer.apply_chat_template(  # type: ignore[no-untyped-call]
            [{"role": "user", "content": prompt}], add_generation_prompt=True
        )
        result: str = generate(llm, tokenizer, prompt=templated, max_tokens=_MAX_TOKENS)
        return result

    return run
