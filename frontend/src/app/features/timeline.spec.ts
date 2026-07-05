import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { of, throwError } from 'rxjs';
import { vi } from 'vitest';

import { Timeline, continuationTurnIds } from './timeline';
import { RecallApi } from '../recall-api';
import { Conversation, ConversationPage } from '../models';

function conv(start: string, end = start, extra: Partial<Conversation> = {}): Conversation {
  return {
    start,
    end,
    turnCount: 1,
    speakers: [],
    preview: 'x',
    moments: [
      {
        start,
        end,
        primary: [{ id: 1, start, end, text: 'x', audioUrl: '/a' } as never],
        alternates: [],
        sources: ['usb'],
      },
    ],
    ...extra,
  };
}

// One single-turn moment per tier, for the per-day coverage test.
const moments = (...tiers: string[]) =>
  tiers.map(
    (tier, i) =>
      ({
        start: `2026-06-13T00:0${i}:00Z`,
        end: `2026-06-13T00:0${i}:00Z`,
        primary: [{ id: i, tier } as never],
        alternates: [],
        sources: ['usb'],
      }) as never,
  );

const at = (hhmm: string) => `2026-06-13T${hhmm}:00Z`;
const page = (items: Conversation[], hasMore = false): ConversationPage => ({ items, hasMore });

function setup(
  opts: {
    before?: string;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    conversations?: any;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    correct?: any;
  } = {},
) {
  const navigate = vi.fn().mockResolvedValue(true);
  const open = vi.fn();
  const conversations = opts.conversations ?? vi.fn(() => of(page([])));
  const correct = opts.correct ?? vi.fn(() => of({ newId: 9 }));
  const speakers = vi.fn(() => of({ names: ['Alice', 'Bob', 'Carol', 'Pippijn'] }));
  const assignSpan = vi.fn(() => of({ touched: 1 }));
  const nudgeTurn = vi.fn(() => of({ ok: true }));
  const refineRange = vi.fn(() => of({ ok: true }));
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      {
        provide: RecallApi,
        useValue: {
          conversations,
          correct,
          speakers,
          assignSpan,
          nudgeTurn,
          refineRange,
        },
      },
      { provide: Router, useValue: { navigate } },
      { provide: ActivatedRoute, useValue: {} },
      { provide: MatSnackBar, useValue: { open } },
    ],
  });
  const fixture = TestBed.createComponent(Timeline);
  if (opts.before !== undefined) {
    fixture.componentRef.setInput('before', opts.before);
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return {
    fixture,
    c,
    navigate,
    conversations,
    correct,
    open,
    assignSpan,
    nudgeTurn,
    refineRange,
  };
}

async function settle(fixture: ComponentFixture<Timeline>) {
  fixture.detectChanges(); // run the constructor effect → restore()
  await fixture.whenStable(); // let firstValueFrom(of(...)) resolve
  fixture.detectChanges();
}

describe('Timeline', () => {
  it('loads the latest window and opens the newest conversation', async () => {
    const older = conv(at('09:00'));
    const newest = conv(at('11:00'));
    const { fixture, c, conversations } = setup({
      conversations: vi.fn(() => of(page([older, newest]))),
    });
    await settle(fixture);
    expect(conversations).toHaveBeenCalled();
    expect(c.conversations()).toHaveLength(2);
    expect(c.isExpanded(newest)).toBe(true);
    expect(c.isExpanded(older)).toBe(false);
  });

  it('groups conversations by day', async () => {
    const { fixture, c } = setup({
      conversations: vi.fn(() =>
        of(page([conv(at('09:00')), conv(at('11:00')), conv('2026-06-14T08:00:00Z')])),
      ),
    });
    await settle(fixture);
    const days = c.days();
    expect(days).toHaveLength(2);
    expect(days[0].conversations).toHaveLength(2);
    expect(days[1].conversations).toHaveLength(1);
  });

  it('summarises per-day diarization coverage from the loaded turns', async () => {
    const { fixture, c } = setup({
      conversations: vi.fn(() =>
        of(
          page([
            conv('2026-06-14T11:00:00Z', '2026-06-14T11:00:00Z', {
              moments: moments('diarized', 'diarized'),
            }),
            conv(at('10:00'), at('10:00'), {
              moments: moments('diarized', 'transcribed', 'transcribed'),
            }),
          ]),
        ),
      ),
    });
    await settle(fixture);
    const [done, partial] = c.days();
    expect(c.coverageLabel(done)).toBe('diarized');
    expect(c.coverageDone(done)).toBe(true);
    expect(c.coverageLabel(partial)).toBe('33% diarized');
    expect(c.coverageDone(partial)).toBe(false);
  });

  it('Load earlier prepends the older page and records the cursor in the URL', async () => {
    const conversations = vi.fn((_limit: number, before?: string) => {
      if (!before) {
        return of(page([conv(at('09:00')), conv(at('11:00'))], true));
      }
      if (before === at('09:00')) {
        return of(page([conv(at('07:00')), conv(at('08:00'))], false));
      }
      return of(page([]));
    });
    const { fixture, c, navigate } = setup({ conversations });
    await settle(fixture);

    await c.loadEarlier();

    expect(c.conversations().map((x: Conversation) => x.start)).toEqual([
      at('07:00'),
      at('08:00'),
      at('09:00'),
      at('11:00'),
    ]);
    // URL records the before we paged from, URL-safe (Z form), as a position
    // (replaceUrl), not a history step — so reload fetches exactly this window.
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({
        queryParams: { before: '2026-06-13T09:00:00.000Z' },
        replaceUrl: true,
      }),
    );
  });

  it('Load later appends the newer page at the bottom (forward paging from a deep-link)', async () => {
    const conversations = vi.fn((_limit: number, before?: string, after?: string) => {
      if (before === at('09:00')) {
        return of(page([conv(at('09:00')), conv(at('10:00'))]));
      }
      if (after === at('10:00')) {
        return of(page([conv(at('11:00')), conv(at('12:00'))], false));
      }
      return of(page([]));
    });
    // Land on a past window → there's newer history above it.
    const { fixture, c, navigate } = setup({ before: at('09:00'), conversations });
    await settle(fixture);
    expect(c.hasNewer()).toBe(true);

    await c.loadLater();

    expect(c.conversations().map((x: Conversation) => x.start)).toEqual([
      at('09:00'),
      at('10:00'),
      at('11:00'),
      at('12:00'),
    ]);
    expect(c.hasNewer()).toBe(false); // reached the present
    // …so the URL cursor is cleared — a reload now shows the latest window.
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { before: null }, replaceUrl: true }),
    );
  });

  it('Load later partway records the forward edge in the URL (so reload restores it)', async () => {
    const conversations = vi.fn((_limit: number, before?: string, after?: string) => {
      if (before === at('09:00')) {
        return of(page([conv(at('09:00')), conv(at('10:00'))]));
      }
      if (after === at('10:00')) {
        // hasMore = true → still short of the present
        return of(page([conv(at('11:00'), at('11:30')), conv(at('12:00'), at('12:30'))], true));
      }
      return of(page([]));
    });
    const { fixture, c, navigate } = setup({ before: at('09:00'), conversations });
    await settle(fixture);

    await c.loadLater();

    // URL records the forward edge (the newest conv's end), not the deep window,
    // and Load-later stays available because more newer history remains.
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({
        queryParams: { before: '2026-06-13T12:30:00.000Z' },
        replaceUrl: true,
      }),
    );
    expect(c.hasNewer()).toBe(true);
  });

  it('the latest window has no newer history to load', async () => {
    const { fixture, c } = setup({
      conversations: vi.fn(() => of(page([conv(at('09:00')), conv(at('11:00'))]))),
    });
    await settle(fixture);
    expect(c.hasNewer()).toBe(false);
  });

  it('reload with a cursor fetches that one window directly (no replay)', async () => {
    const conversations = vi.fn((_limit: number, before?: string) =>
      before === at('11:00') ? of(page([conv(at('09:00')), conv(at('11:00'))])) : of(page([])),
    );
    // Land directly on ?before=11:00 (a reload/share of a scrolled-back position).
    const { fixture, c } = setup({ before: at('11:00'), conversations });
    await settle(fixture);

    // One request at the cursor — no walking back from latest (which is capped).
    expect(c.conversations().map((x: Conversation) => x.start)).toEqual([at('09:00'), at('11:00')]);
    expect(conversations).toHaveBeenCalledTimes(1);
    expect(conversations).toHaveBeenCalledWith(200, at('11:00'));
  });

  it('does not auto-expand when restoring an older (cursored) view', async () => {
    const newest = conv(at('11:00'));
    const { fixture, c } = setup({
      before: at('11:00'),
      conversations: vi.fn(() => of(page([conv(at('09:00')), newest]))),
    });
    await settle(fixture);
    expect(c.isExpanded(newest)).toBe(false);
  });

  it('Jump to latest clears the cursor (?before=null)', () => {
    const { c, navigate } = setup({ before: at('12:00') });
    c.jumpToLatest();
    expect(navigate).toHaveBeenCalledWith(
      [],
      expect.objectContaining({ queryParams: { before: null }, replaceUrl: true }),
    );
  });

  it('toggle expands and collapses a conversation', () => {
    const { c } = setup();
    const item = conv(at('09:00'));
    expect(c.isExpanded(item)).toBe(false);
    c.toggle(item);
    expect(c.isExpanded(item)).toBe(true);
    c.toggle(item);
    expect(c.isExpanded(item)).toBe(false);
  });

  it('range shows a span, or a single time when start equals end', () => {
    const { c } = setup();
    expect(c.range(conv(at('09:00'), at('09:05')))).toContain('–');
    expect(c.range(conv(at('09:00'), at('09:00')))).not.toContain('–');
  });

  it('formats voice-match strength as a percentage, blank when none', () => {
    const { c } = setup();
    expect(c.pct({ speakerConfidence: 0.31 })).toBe('31%');
    expect(c.pct({ speakerConfidence: 0.5 })).toBe('50%');
    expect(c.pct({ speakerConfidence: null })).toBe('');
  });

  it('relabel files a correction with the chosen speaker and tags the turn locally', () => {
    const { c, correct } = setup();
    const t = {
      id: 5,
      text: 'hallo',
      language: 'nl',
      speaker: 'Alice',
      speakerConfirmed: false,
      speakerConfidence: 0.3,
    };
    expect(c.who(t)).toBe('Alice'); // the auto-guess, until corrected
    expect(c.tagged(t)).toBe(false);

    c.relabel(t, 'Carol');

    // Keeps the text, sets the speaker — which also enrols the voiceprint.
    expect(correct).toHaveBeenCalledWith(5, 'hallo', { speaker: 'Carol', language: 'nl' });
    expect(c.who(t)).toBe('Carol');
    expect(c.tagged(t)).toBe(true);
  });

  it('reports a failed speaker tag via the snackbar (and does not tag locally)', () => {
    const { c, open } = setup({ correct: vi.fn(() => throwError(() => new Error('nope'))) });
    const t = { id: 7, text: 'x', language: null, speaker: null, speakerConfirmed: false };
    c.relabel(t, 'Carol');
    expect(open).toHaveBeenCalled();
    expect(c.tagged(t)).toBe(false);
  });

  it('splits the selected phrase onto a speaker, posting to the turn’s source', () => {
    const { c, assignSpan } = setup();
    c.span.set({ startTurn: 28460, startChar: 100, endTurn: 28460, endChar: 114 });
    c.spanSource.set('usb');
    c.assignSelectedSpan('Pippijn');
    expect(assignSpan).toHaveBeenCalledWith('usb', {
      startTurn: 28460,
      startChar: 100,
      endTurn: 28460,
      endChar: 114,
      name: 'Pippijn',
    });
    expect(c.span()).toBeNull(); // cleared after a successful split
  });

  it('does not split without a source (a cross-source selection)', () => {
    const { c, assignSpan } = setup();
    c.span.set({ startTurn: 1, startChar: 0, endTurn: 1, endChar: 3 });
    c.spanSource.set(null);
    c.assignSelectedSpan('Pippijn');
    expect(assignSpan).not.toHaveBeenCalled();
  });

  it('trims a turn boundary by ear: posts the nudge and re-fetches the clip', () => {
    const { c, nudgeTurn } = setup();
    const t = { id: 40022, audioUrl: '/api/audio/40022', text: 'Ja.' };
    c.startTrim(t);
    expect(c.isTrimming(t)).toBe(true);
    const before = c.audioSrc(t); // cache-busted while trimming
    c.nudgeTurn(t, 'start', -0.1);
    expect(nudgeTurn).toHaveBeenCalledWith(40022, 'start', -0.1);
    expect(c.audioSrc(t)).not.toBe(before); // version bumped → re-fetch the re-sliced clip
    c.stopTrim();
    expect(c.isTrimming(t)).toBe(false);
    expect(c.audioSrc(t)).toBe('/api/audio/40022'); // plain url once done
  });
});

describe('continuationTurnIds', () => {
  const BASE = Date.UTC(2026, 5, 13, 12, 0, 0);
  const iso = (s: number) => new Date(BASE + s * 1000).toISOString();
  const t = (id: number, speaker: string | null, confirmed: boolean, a: number, b: number) => ({
    id,
    speaker,
    confirmed,
    start: iso(a),
    end: iso(b),
  });

  it('coalesces consecutive confirmed same-speaker turns within 1s', () => {
    const set = continuationTurnIds([
      t(1, 'Alice', true, 0, 2),
      t(2, 'Alice', true, 2, 4), // touches → continuation
      t(3, 'Alice', true, 4.5, 6), // 0.5s gap → continuation
    ]);
    expect([...set]).toEqual([2, 3]);
  });

  it('never coalesces across different speakers or unknowns', () => {
    const set = continuationTurnIds([
      t(1, 'Alice', true, 0, 2),
      t(2, 'Pippijn', true, 2, 4), // different speaker
      t(3, null, false, 4, 6), // unknown
      t(4, null, false, 6, 8), // unknown↔unknown never coalesces
    ]);
    expect([...set]).toEqual([]);
  });

  it('does not coalesce an unconfirmed (guessed) speaker', () => {
    const set = continuationTurnIds([
      t(1, 'Alice', true, 0, 2),
      t(2, 'Alice', false, 2, 4), // same name but only a guess
    ]);
    expect([...set]).toEqual([]);
  });

  it('breaks the run at a gap of 1s or more', () => {
    const set = continuationTurnIds([
      t(1, 'Alice', true, 0, 2),
      t(2, 'Alice', true, 3, 5), // exactly 1.0s gap → not a continuation
    ]);
    expect([...set]).toEqual([]);
  });

  it('treats an overlap (negative gap from a trim) as continuing', () => {
    const set = continuationTurnIds([
      t(1, 'Alice', true, 0, 2),
      t(2, 'Alice', true, 1.8, 4), // overlaps by 0.2s
    ]);
    expect([...set]).toEqual([2]);
  });
});

describe('refineConversation', () => {
  it('queues a refine of the conversation window on its primary source', () => {
    const { c, refineRange, open } = setup();
    const conv = {
      start: '2026-01-15T09:30:00+00:00',
      end: '2026-01-15T09:48:00+00:00',
      moments: [{ primary: [{ id: 5, source: 'usb' }] }],
    };
    c.refineConversation(conv);
    expect(refineRange).toHaveBeenCalledWith(
      'usb',
      '2026-01-15T09:30:00+00:00',
      '2026-01-15T09:48:00+00:00',
    );
    expect(open).toHaveBeenCalled(); // confirmation snackbar
  });

  it('does nothing when the conversation has no source', () => {
    const { c, refineRange } = setup();
    c.refineConversation({ start: 'x', end: 'y', moments: [{ primary: [{ id: 5 }] }] });
    expect(refineRange).not.toHaveBeenCalled();
  });
});
