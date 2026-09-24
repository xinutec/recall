/** Map a text selection onto turns: each turn is a `span.t` with `data-id` and
 * `data-source`. */

export interface SpanSel {
  readonly startTurn: number;
  readonly startChar: number;
  readonly endTurn: number;
  readonly endChar: number;
}

interface Endpoint {
  readonly turn: number;
  readonly char: number;
  readonly source: string | null;
}

function endpointFor(node: Node, offset: number): Endpoint | null {
  // An endpoint can be a comment node, where `closest` would throw.
  const el = node.nodeType === Node.TEXT_NODE ? node.parentElement : node instanceof Element ? node : null;
  const span = el?.closest('span.t');
  const id = span instanceof HTMLElement ? span.dataset['id'] : undefined;
  if (!(span instanceof HTMLElement) || id === undefined) {
    return null;
  }
  const len = (span.textContent ?? '').trimEnd().length; // past a template trailing space
  return {
    turn: Number(id),
    char: Math.max(0, Math.min(offset, len)),
    source: span.dataset['source'] ?? null,
  };
}

/** The span and its source; null outside a turn or across two sources, which one
 * split cannot cover. */
export function resolveSelection(
  range: Range,
): { span: SpanSel; source: string | null } | null {
  const a = endpointFor(range.startContainer, range.startOffset);
  const b = endpointFor(range.endContainer, range.endOffset);
  if (!a || !b) {
    return null;
  }
  if (a.source && b.source && a.source !== b.source) {
    return null;
  }
  return {
    span: { startTurn: a.turn, startChar: a.char, endTurn: b.turn, endChar: b.char },
    source: a.source ?? b.source,
  };
}
