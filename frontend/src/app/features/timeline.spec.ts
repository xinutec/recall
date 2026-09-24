import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { of } from 'rxjs';
import { vi } from 'vitest';

import { Timeline } from './timeline';
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
  } = {},
) {
  const navigate = vi.fn().mockResolvedValue(true);
  const open = vi.fn();
  const conversations = opts.conversations ?? vi.fn(() => of(page([])));
  const speakers = vi.fn(() => of({ names: ['Alice', 'Bob', 'Carol', 'Pippijn'] }));
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      {
        provide: RecallApi,
        useValue: {
          conversations,
          speakers,
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
    open,
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
    expect(c.convos()).toHaveLength(2);
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

    expect(c.convos().map((x: Conversation) => x.start)).toEqual([
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

    expect(c.convos().map((x: Conversation) => x.start)).toEqual([
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
    expect(c.convos().map((x: Conversation) => x.start)).toEqual([at('09:00'), at('11:00')]);
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
});
