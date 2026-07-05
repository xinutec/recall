import { describe, expect, it } from 'vitest';

import { wordDiff } from './word-diff';

describe('wordDiff', () => {
  it('flags only the words that differ', () => {
    const { a, b } = wordDiff('the quick brown fox', 'the slow brown fox');
    expect(a.map((t) => t.text)).toEqual(['the', 'quick', 'brown', 'fox']);
    expect(a.map((t) => t.changed)).toEqual([false, true, false, false]);
    expect(b.map((t) => t.changed)).toEqual([false, true, false, false]);
  });

  it('ignores case and punctuation when matching', () => {
    const { a, b } = wordDiff('Mask. Thank you', 'mask thank You');
    expect(a.every((t) => !t.changed)).toBe(true);
    expect(b.every((t) => !t.changed)).toBe(true);
  });

  it('marks an insertion on one side only', () => {
    const { a, b } = wordDiff('hello world', 'hello there world');
    expect(a.map((t) => t.changed)).toEqual([false, false]);
    const there = b.find((t) => t.text === 'there');
    expect(there?.changed).toBe(true);
  });

  it('marks everything changed when there is no overlap', () => {
    const { a, b } = wordDiff('alpha beta', 'gamma delta');
    expect(a.every((t) => t.changed)).toBe(true);
    expect(b.every((t) => t.changed)).toBe(true);
  });

  it('handles empty input', () => {
    expect(wordDiff('', '')).toEqual({ a: [], b: [] });
  });
});
