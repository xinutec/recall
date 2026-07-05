/**
 * Map a DOM text selection onto transcript turns — the shared core of the
 * drag-select-and-assign gesture used by both the session view and the timeline.
 *
 * Each turn's text is rendered in a `span.t` carrying `data-id` (the turn id) and,
 * optionally, `data-source` (the recording it belongs to). This resolves a `Range`'s
 * endpoints back to turn ids + character offsets, and the single source the resulting
 * split must be posted to. A selection that crosses two sources can't be one split, so
 * it resolves to null.
 */

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
  const el = node.nodeType === Node.TEXT_NODE ? node.parentElement : (node as Element);
  const span = el?.closest('span.t') as HTMLElement | null;
  const id = span?.dataset['id'];
  if (!span || id === undefined) {
    return null;
  }
  const len = (span.textContent ?? '').trimEnd().length; // past a template trailing space
  return {
    turn: Number(id),
    char: Math.max(0, Math.min(offset, len)),
    source: span.dataset['source'] ?? null,
  };
}

/** The selected span plus the source it belongs to, or null if either endpoint isn't in
 * a turn or the selection crosses sources (which can't be split as one). */
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
