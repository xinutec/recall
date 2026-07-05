import { resolveSelection } from './selection-span';

/** Build a `span.t` with the given text + dataset and append it to the body. */
function turnSpan(id: number, text: string, source?: string): HTMLSpanElement {
  const span = document.createElement('span');
  span.className = 't';
  span.dataset['id'] = String(id);
  if (source !== undefined) {
    span.dataset['source'] = source;
  }
  span.textContent = text;
  document.body.append(span);
  return span;
}

function rangeOver(node: Node, start: number, end: number): Range {
  const r = document.createRange();
  r.setStart(node.firstChild!, start);
  r.setEnd(node.firstChild!, end);
  return r;
}

afterEach(() => {
  document.body.innerHTML = '';
});

describe('resolveSelection', () => {
  it('maps a sub-phrase to its turn id, char offsets, and source', () => {
    const span = turnSpan(7, 'a list of errands ', 'usb');
    const got = resolveSelection(rangeOver(span, 0, 5)); // "a list"
    expect(got).toEqual({
      span: { startTurn: 7, startChar: 0, endTurn: 7, endChar: 5 },
      source: 'usb',
    });
  });

  it('clamps past a trailing space (the template adds one)', () => {
    const span = turnSpan(7, 'hello ', 'usb'); // length-with-space 6, trimmed 5
    const got = resolveSelection(rangeOver(span, 0, 6));
    expect(got?.span.endChar).toBe(5);
  });

  it('returns null when an endpoint is not inside a turn', () => {
    const span = turnSpan(7, 'hi', 'usb');
    const outside = document.createElement('div');
    outside.textContent = 'name';
    document.body.append(outside);
    const r = document.createRange();
    r.setStart(span.firstChild!, 0);
    r.setEnd(outside.firstChild!, 2);
    expect(resolveSelection(r)).toBeNull();
  });

  it('refuses a selection that crosses two sources', () => {
    const a = turnSpan(1, 'one', 'usb');
    const b = turnSpan(2, 'two', 'meeting-x');
    const r = document.createRange();
    r.setStart(a.firstChild!, 0);
    r.setEnd(b.firstChild!, 3);
    expect(resolveSelection(r)).toBeNull();
  });

  it('source is null when the span carries none (session view)', () => {
    const span = turnSpan(7, 'hi'); // no data-source
    expect(resolveSelection(rangeOver(span, 0, 2))?.source).toBeNull();
  });
});
