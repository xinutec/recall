"""The household vocabulary → Whisper's initial_prompt (proper-noun biasing).

The cheap accuracy lever pipeline.md ranks above fine-tuning: Whisper biases its
decoding toward tokens seen in the prompt, so feeding it the names, places and
terms this household actually says makes it spell them right — no training, no
regression risk. The prompt is rebuilt from the store on every transcription
call, so a term added in the UI takes effect on the next segment.
"""

from __future__ import annotations

from recall.store import Store

# Whisper reserves 224 tokens for the prompt; stay comfortably under it so the
# bias list can never crowd out real left-context.
_MAX_PROMPT_CHARS = 600


def build_initial_prompt(store: Store) -> str | None:
    """A glossary sentence for Whisper's initial_prompt, or None when empty.

    Enrolled speaker names first (short, highest value), then the explicit
    vocabulary, joined as a plain comma list — Whisper only needs to SEE the
    tokens; prose adds nothing.
    """
    seen: dict[str, None] = {}
    for name in store.known_speaker_names():
        seen.setdefault(name, None)
    for entry in store.vocabulary_terms():
        seen.setdefault(entry.term, None)
    if not seen:
        return None
    prompt = ""
    for term in seen:
        extended = term if not prompt else f"{prompt}, {term}"
        if len(extended) > _MAX_PROMPT_CHARS:
            break
        prompt = extended
    return prompt or None
