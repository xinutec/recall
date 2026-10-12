import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { Sheets } from '@xinutec/ui-scaffold';
import { Subject } from 'rxjs';
import { vi } from 'vitest';

import { Turns, blocksOf, runsOf } from './turns';
import { LineSheetData, LineSheetResult } from './line-sheet';
import { Player } from './player';
import { Moment, Transcript } from '../models';
import { timeOfDay } from '../format';

function turn(o: Partial<Transcript>): Transcript {
  return {
    id: 0,
    start: '2026-01-15T10:33:00Z',
    end: '2026-01-15T10:33:05Z',
    text: 't',
    language: 'en',
    speaker: null,
    speakerConfirmed: false,
    wordsChecked: false,
    speakerConfidence: null,
    confidence: null,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    hiddenAs: null,
    audioUrl: '/a',
    source: 'm',
    cluster: null,
    ...o,
  };
}

const said = (id: number, speaker: string | null, o: Partial<Transcript> = {}): Transcript =>
  turn({ id, speaker, speakerConfirmed: !!speaker, audioUrl: `/api/audio/${id}`, ...o });

const line = (primary: Transcript, alternates: Transcript[] = []): Moment => ({
  start: primary.start,
  end: primary.end,
  primary,
  alternates,
  sources: [...new Set([primary, ...alternates].map((t) => t.source ?? ''))],
});

/** One moment per line, as the server folds a mic's lines. */
const lines = (...shown: Transcript[]): Moment[] => shown.map((t) => line(t));

async function setup(moments: Moment[], roster: string[] = []) {
  const closed = new Subject<LineSheetResult | boolean | undefined>();
  const sheet = {
    open: vi.fn((_: unknown, config: { data: LineSheetData }) => {
      void config;
      return { afterDismissed: () => closed };
    }),
  };
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: Sheets, useValue: sheet },
    ],
  });
  const fixture = TestBed.createComponent(Turns);
  fixture.componentRef.setInput('moments', moments);
  fixture.componentRef.setInput('roster', roster);
  const changed = vi.fn();
  fixture.componentInstance.changed.subscribe(changed);
  fixture.detectChanges();
  await fixture.whenStable();
  const player = TestBed.inject(Player);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
  const c = fixture.componentInstance as any;
  return { fixture, c, changed, sheet, closed, player };
}

function stubMedia() {
  const play = vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
  const pause = vi.spyOn(HTMLMediaElement.prototype, 'pause').mockReturnValue(undefined);
  return { play, pause };
}

describe('runsOf', () => {
  it('joins consecutive turns of one speaker', () => {
    const runs = runsOf([said(1, 'P'), said(2, 'P'), said(3, 'D'), said(4, 'P')]);
    expect(runs.map((r) => r.speaker)).toEqual(['P', 'D', 'P']);
    expect(runs[0].turns.map((t) => t.id)).toEqual([1, 2]);
  });

  it('keeps a guess apart from the confirmed name, with its strength', () => {
    const guess = turn({ id: 2, speaker: 'P', speakerConfidence: 0.31 });
    const runs = runsOf([said(1, 'P'), guess]);
    expect(runs.map((r) => [r.speaker, r.confirmed, r.guess])).toEqual([
      ['P', true, null],
      ['P', false, 0.31],
    ]);
  });

  it('names an unnamed turn by its session voice, else unknown', () => {
    const voices = new Map([['A', 'Voice 1']]);
    const runs = runsOf([turn({ id: 1, cluster: 'A' }), turn({ id: 2 })], voices);
    expect(runs.map((r) => r.speaker)).toEqual(['Voice 1', 'unknown']);
  });
});

describe('blocksOf', () => {
  const excluded = (id: number, start: string, end: string) =>
    said(id, null, { start, end, hidden: 'excluded', hiddenAs: 'excluded' });

  it('folds a stretch a person excluded into one row from its first start to its last end', () => {
    const blocks = blocksOf([
      said(1, 'P'),
      excluded(2, '2026-01-15T14:02:00Z', '2026-01-15T14:05:00Z'),
      excluded(3, '2026-01-15T14:10:00Z', '2026-01-15T14:19:00Z'),
      said(4, 'P'),
    ]);
    expect(blocks.map((b) => b.kind)).toEqual(['run', 'excluded', 'run']);
    expect(blocks[1]).toMatchObject({
      from: '2026-01-15T14:02:00Z',
      to: '2026-01-15T14:19:00Z',
    });
  });

  it('keeps one speaker’s lines apart on either side of an excluded stretch', () => {
    const blocks = blocksOf([
      said(1, 'P'),
      excluded(2, '2026-01-15T14:02:00Z', '2026-01-15T14:05:00Z'),
      said(3, 'P'),
    ]);
    expect(blocks.filter((b) => b.kind === 'run')).toHaveLength(2);
  });
});

describe('Turns', () => {
  afterEach(() => vi.restoreAllMocks());

  it('offers the names in view, else the roster', async () => {
    const named = await setup(lines(said(1, 'P'), said(2, null)), ['A', 'B']);
    expect(named.c.palette()).toEqual(['P']);
    TestBed.resetTestingModule();
    const bare = await setup(lines(said(1, null)), ['A', 'B']);
    expect(bare.c.palette()).toEqual(['A', 'B']);
  });

  it('gives each speaker a distinct colour', async () => {
    const { c } = await setup(lines(said(1, 'P'), said(2, 'D')));
    expect(c.colourFor('P')).toMatch(/^#/);
    expect(c.colourFor('P')).not.toBe(c.colourFor('D'));
  });

  it('a tap opens the line’s sheet, and a write there refreshes', async () => {
    const { c, changed, sheet, closed } = await setup(lines(said(1, 'P', { text: 'hallo' })));
    const t = c.turns()[0];
    c.openLine(t);
    expect(c.selected()).toBe(1);
    const data = sheet.open.mock.calls[0][1].data;
    expect(data).toMatchObject({ turn: t, speaker: 'P', confirmed: true, names: ['P'] });
    closed.next(true);
    expect(c.selected()).toBeNull();
    expect(changed).toHaveBeenCalled();
  });

  it('closing the sheet without a write refreshes nothing', async () => {
    const { c, changed, closed } = await setup(lines(said(1, 'P')));
    c.openLine(c.turns()[0]);
    closed.next(undefined);
    expect(changed).not.toHaveBeenCalled();
  });

  it('a transcribed line is grey', async () => {
    const { c } = await setup(lines(said(1, 'P', { tier: 'transcribed' })));
    expect(c.provisional(c.turns()[0])).toBe(true);
  });

  it('plays a run as one span, pauses, then resumes in place', async () => {
    const { play, pause } = stubMedia();
    const { c, player } = await setup(lines(said(1, 'P'), said(2, 'P')));
    const run = c.runs()[0];
    c.togglePlay(run);
    expect(player.playing()).toBe('run:1');
    c.togglePlay(run);
    expect(player.playing()).toBeNull();
    c.togglePlay(run);
    expect(play).toHaveBeenCalledTimes(2);
    expect(pause).toHaveBeenCalledTimes(1);
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the player's private audio element, in a test
    expect((player as any).audio.src).toContain('/api/audio-span?from_id=1&to_id=2');
    player.stop();
  });

  it('stops its own clip when destroyed, and leaves another view’s alone', async () => {
    stubMedia();
    const { fixture, c, player } = await setup(lines(said(1, 'P')));
    const other = TestBed.createComponent(Turns);
    other.componentRef.setInput('moments', lines(said(2, 'P')));
    other.detectChanges();
    c.togglePlay(c.runs()[0]);
    other.destroy();
    expect(player.playing()).toBe('run:1');
    fixture.destroy();
    expect(player.playing()).toBeNull();
  });

  it('flags a line the mics disagree on, and counts the mics', async () => {
    const usb = said(1, 'P', { source: 'usb' });
    const phone = said(2, 'D', { source: 'phone' });
    const { c } = await setup([line(usb, [phone])]);
    expect(c.disputed(usb)).toBe(true);
    expect(c.mics(usb)).toBe(2);
    expect(c.alternates(usb)).toEqual([phone]);
  });

  it('shows a line’s own moment’s versions, counting its mic once', async () => {
    const first = said(1, 'P', { source: 'usb' });
    const second = said(2, 'P', { source: 'usb' });
    const same = said(3, 'D', { source: 'phone' });
    const copy = said(5, 'P', { source: 'usb' });
    const later = said(4, 'D', { source: 'phone' });
    const { c } = await setup([line(first, [same, copy]), line(second, [later])]);
    expect(c.alternates(first)).toEqual([same, copy]);
    // The mic's own second copy is not a second mic.
    expect(c.mics(first)).toBe(2);
    expect(c.alternates(second)).toEqual([later]);
  });

  it('does not flag a machine guess that differs from a person’s name', async () => {
    const named = said(1, 'P', { source: 'usb' });
    const guessed = said(2, null, { source: 'phone', speaker: 'D', speakerConfirmed: false });
    const { c } = await setup([line(named, [guessed])]);
    expect(c.disputed(named)).toBe(false);
  });

  it('flags only a disagreement with a confirmed name', async () => {
    const usb = said(1, null, { source: 'usb', speaker: 'P', speakerConfirmed: false });
    const phone = said(2, null, { source: 'phone', speaker: 'D', speakerConfirmed: false });
    const { c } = await setup([line(usb, [phone])]);
    // Two guesses that differ are two guesses, not a finding.
    expect(c.disputed(usb)).toBe(false);
  });

  it('tags the mic where it changes, only when several are in view', async () => {
    const one = await setup(lines(said(1, 'P'), said(2, 'P')));
    expect(one.c.sourceTags().size).toBe(0);
    TestBed.resetTestingModule();
    const two = await setup([
      line(said(1, 'P', { source: 'usb' })),
      line(said(2, 'P', { source: 'usb' })),
      line(said(3, 'P', { source: 'phone' })),
    ]);
    expect([...two.c.sourceTags().entries()]).toEqual([
      [1, 'usb'],
      [3, 'phone'],
    ]);
  });

  it('excludes a stretch picked from one line to a later one, and undoes it from its row', async () => {
    const at = (start: string, end: string) => ({ start, end });
    const first = said(1, 'P', at('2026-01-15T14:02:00Z', '2026-01-15T14:02:05Z'));
    const before = said(2, 'P', at('2026-01-15T14:01:00Z', '2026-01-15T14:01:05Z'));
    const last = said(3, 'P', at('2026-01-15T14:19:00Z', '2026-01-15T14:19:05Z'));
    const { c, changed, closed, fixture } = await setup(lines(before, first, last));
    const http = TestBed.inject(HttpTestingController);

    c.openLine(first);
    closed.next('excludeFrom');
    expect(c.excluding()).toMatchObject({ from: first, to: null });
    expect(changed).not.toHaveBeenCalled();

    c.openLine(before);
    expect(c.excluding().to).toBeNull();
    c.openLine(last);
    expect(c.excluding().to).toBe(last);
    fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain(
      `Exclude ${timeOfDay(first.start)}–${timeOfDay(last.end)}?`,
    );

    c.confirmExclude();
    const req = http.expectOne('/api/exclude');
    expect(req.request.body).toEqual({ from: 1, to: 3 });
    req.flush({ ok: true });
    expect(c.excluding()).toBeNull();
    expect(changed).toHaveBeenCalledTimes(1);

    c.undoExclude(first);
    const undo = http.expectOne('/api/exclude/undo');
    expect(undo.request.body).toEqual({ id: 1 });
    undo.flush({ ok: true });
    expect(changed).toHaveBeenCalledTimes(2);
    http.verify();
  });

  it('cancelling a stretch leaves the lines as they were', async () => {
    const { c, closed } = await setup(lines(said(1, 'P'), said(2, 'P')));
    c.openLine(c.turns()[0]);
    closed.next('excludeFrom');
    c.cancelExclude();
    expect(c.excluding()).toBeNull();
    TestBed.inject(HttpTestingController).verify();
  });
});
