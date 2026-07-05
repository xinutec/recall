"""Ask-the-archive: retrieve relevant turns (FTS), answer grounded in them.

Retrieval-then-generation over the transcript store — no embeddings index, no
cloud. FTS5 already covers the archive; the question's content words become an
OR query, the hits (plus their ids) go into the prompt, and the answer must cite
what it used. Without evidence there is NO generation: an LLM answering from
thin air is exactly the failure mode a memory aid cannot have.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from recall.context import household_context_block
from recall.llm import Generator
from recall.store import Store, TranscriptSegment

_RETRIEVE_LIMIT = 40

# Question words and glue that would only add FTS noise. Both household
# languages, deliberately small — an over-aggressive list starves retrieval.
_STOPWORDS = frozenset(
    {
        # English
        "a",
        "an",
        "and",
        "are",
        "be",
        "but",
        "can",
        "could",
        "did",
        "do",
        "does",
        "for",
        "from",
        "had",
        "has",
        "have",
        "how",
        "i",
        "in",
        "is",
        "it",
        "of",
        "on",
        "or",
        "say",
        "said",
        "she",
        "he",
        "t",
        "that",
        "the",
        "them",
        "then",
        "there",
        "they",
        "this",
        "to",
        "was",
        "we",
        "what",
        "when",
        "where",
        "which",
        "who",
        "why",
        "will",
        "with",
        "you",
        "your",
        # Dutch
        "de",
        "een",
        "en",
        "het",
        "ik",
        "je",
        "van",
        "voor",
        "wat",
        "wanneer",
        "wie",
        "waar",
        "dat",
        "dit",
        "zij",
        "hij",
        "ze",
    }
)

_PROMPT = """\
{context}You answer one question about what was said in a household's recorded \
conversations, for the people who live there. Use ONLY the transcript excerpts \
below — do not invent details, names, dates or times. If the excerpts do not \
contain the answer, say plainly that the recordings don't show it. Answer in \
the language of the question, in 1-4 sentences.

Question: {question}

Excerpts (times are local, speaker names where known):
{turns}
"""


@dataclass(frozen=True)
class AskResult:
    """A grounded answer plus the turn ids it drew on (for deep links)."""

    answer: str
    sources: tuple[int, ...]


def _fts_query(question: str) -> str:
    words = [
        w
        for w in re.findall(r"[\w']+", question.lower())
        if w not in _STOPWORDS and len(w) > 1
    ]
    # OR of the content words: recall beats precision here — the model reads the
    # excerpts and discards irrelevant ones, but it cannot read what FTS dropped.
    return " OR ".join(dict.fromkeys(words))


def retrieve(store: Store, question: str) -> list[TranscriptSegment]:
    """Turns plausibly relevant to `question`, oldest first (reading order)."""
    query = _fts_query(question)
    if not query:
        return []
    hits = store.search(query, limit=_RETRIEVE_LIMIT)
    return sorted(hits, key=lambda t: t.start)


def build_ask_prompt(
    question: str, turns: list[TranscriptSegment], *, context: str = ""
) -> str:
    lines = [
        f"[{t.start.astimezone().strftime('%Y-%m-%d %H:%M')}] "
        f"{t.speaker_label or '?'}: {t.text}"  # confirmed labels only, no guesses
        for t in turns
    ]
    return _PROMPT.format(context=context, question=question, turns="\n".join(lines))


def answer_question(
    store: Store, generator: Generator, question: str
) -> AskResult | None:
    """Answer `question` from the archive, or None when retrieval finds nothing
    (the caller renders that honestly rather than letting a model improvise)."""
    turns = retrieve(store, question)
    if not turns:
        return None
    prompt = build_ask_prompt(question, turns, context=household_context_block(store))
    answer = generator(prompt).strip()
    return AskResult(answer=answer, sources=tuple(t.id for t in turns))
