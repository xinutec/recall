"""Household background facts for the LLM prompts (summaries + ask).

The facts live in the DB (settings key `household_context`, edited on the
Labels page) — never in the repo, which stays PII-free. They give the model
stable ground truth it otherwise guesses at (who lives here, pronouns,
recurring places), framed explicitly as background so it can't be mistaken
for transcript content.
"""

from __future__ import annotations

from recall.store import Store

CONTEXT_KEY = "household_context"

_BLOCK = """\
Background facts about the household (given by its members; \
not part of the transcript):
{facts}

"""


def household_context_block(store: Store) -> str:
    """The prompt block carrying the stored facts, or "" when none are set."""
    facts = store.get_setting(CONTEXT_KEY)
    return _BLOCK.format(facts=facts) if facts else ""
