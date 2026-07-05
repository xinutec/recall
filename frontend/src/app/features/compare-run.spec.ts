import { describe, expect, it, vi } from 'vitest';
import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute } from '@angular/router';

import { CompareRun, buildEvidenceCards, buildSegmentCards } from './compare-run';
import { AbCompareScore, AbCompareSegmentDiff } from '../models';

function score(over: Partial<AbCompareScore>): AbCompareScore {
  return {
    correctionId: 1,
    truth: 'the quick brown fox',
    textA: 'the quick brown fox',
    textB: 'the quick brown fox',
    werA: 0,
    werB: 0,
    audioUrl: '/api/correction/1/audio',
    ...over,
  };
}

describe('buildEvidenceCards', () => {
  it('sorts by the biggest A↔B WER gap first', () => {
    const cards = buildEvidenceCards([
      score({ correctionId: 1, werA: 0.1, werB: 0.15 }), // gap 0.05
      score({ correctionId: 2, werA: 0.05, werB: 0.85 }), // gap 0.80
      score({ correctionId: 3, werA: 0.2, werB: 0.25 }), // gap 0.05
    ]);
    expect(cards.map((c) => c.score.correctionId)[0]).toBe(2);
  });

  it('names the winner as the model with the lower WER', () => {
    const [bWins, aWins, tie] = buildEvidenceCards([
      score({ correctionId: 1, werA: 0.5, werB: 0.1 }),
      score({ correctionId: 2, werA: 0.1, werB: 0.5 }),
      score({ correctionId: 3, werA: 0.3, werB: 0.3 }),
    ]).sort((x, y) => x.score.correctionId - y.score.correctionId);
    expect(bWins.winner).toBe('B');
    expect(aWins.winner).toBe('A');
    expect(tie.winner).toBe('tie');
  });

  it("highlights each model's words that deviate from the ground truth", () => {
    const [card] = buildEvidenceCards([
      score({ truth: 'hello world', textA: 'hello world', textB: 'hello there', werB: 0.5 }),
    ]);
    expect(card.aTokens.every((t) => !t.changed)).toBe(true); // A matches truth
    const changed = card.bTokens.find((t) => t.changed);
    expect(changed?.text).toBe('there'); // B's deviation is flagged
  });
});

describe('buildSegmentCards', () => {
  function diff(over: Partial<AbCompareSegmentDiff>): AbCompareSegmentDiff {
    return {
      audioId: 1,
      start: '2026-01-15T09:33:03+00:00',
      changed: true,
      textA: 'a b c',
      textB: 'a b c',
      ...over,
    };
  }

  it('keeps only the segments that actually differ', () => {
    const cards = buildSegmentCards([
      diff({ audioId: 1, changed: false }),
      diff({ audioId: 2, changed: true, textA: 'a b c', textB: 'a x c' }),
    ]);
    expect(cards.map((c) => c.diff.audioId)).toEqual([2]);
    expect(cards[0].bTokens.find((t) => t.changed)?.text).toBe('x');
  });
});

describe('CompareRun — polling lifecycle', () => {
  it('clears its status poller when the component is destroyed', () => {
    vi.useFakeTimers();
    try {
      TestBed.configureTestingModule({
        providers: [
          provideZonelessChangeDetection(),
          provideHttpClient(),
          provideHttpClientTesting(),
          { provide: ActivatedRoute, useValue: {} },
        ],
      });
      const fixture = TestBed.createComponent(CompareRun);
      fixture.componentRef.setInput('id', '12');
      const withPoller = vi.getTimerCount();
      fixture.destroy();
      expect(vi.getTimerCount()).toBe(withPoller - 1);
    } finally {
      vi.useRealTimers();
    }
  });
});
