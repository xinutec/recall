import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { MatBottomSheet } from '@angular/material/bottom-sheet';
import { Subject } from 'rxjs';
import { vi } from 'vitest';

import { Turns, runsOf } from './turns';
import { LineSheetData } from './line-sheet';
import { Player } from './player';
import { Moment, Transcript } from '../models';

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
    loudness: null,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    audioUrl: '/a',
    source: 'm',
    cluster: null,
    ...o,
  };
}

const said = (id: number, speaker: string | null, o: Partial<Transcript> = {}): Transcript =>
  turn({ id, speaker, speakerConfirmed: !!speaker, audioUrl: `/api/audio/${id}`, ...o });

const moment = (primary: Transcript[], alternates: Transcript[] = []): Moment => ({
  start: primary[0].start,
  end: primary[0].end,
  primary,
  alternates,
  sources: [...new Set([...primary, ...alternates].map((t) => t.source ?? ''))],
});

async function setup(moments: Moment[], roster: string[] = []) {
  const closed = new Subject<boolean | undefined>();
  const sheet = {
    open: vi.fn((_: unknown, config: { data: LineSheetData }) => {
      void config;
      return { afterDismissed: () => closed };
    }),
  };
  TestBed.configureTestingModule({
    providers: [provideZonelessChangeDetection(), { provide: MatBottomSheet, useValue: sheet }],
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

describe('Turns', () => {
  afterEach(() => vi.restoreAllMocks());

  it('offers the names in view, else the roster', async () => {
    const named = await setup([moment([said(1, 'P'), said(2, null)])], ['A', 'B']);
    expect(named.c.palette()).toEqual(['P']);
    TestBed.resetTestingModule();
    const bare = await setup([moment([said(1, null)])], ['A', 'B']);
    expect(bare.c.palette()).toEqual(['A', 'B']);
  });

  it('gives each speaker a distinct colour', async () => {
    const { c } = await setup([moment([said(1, 'P'), said(2, 'D')])]);
    expect(c.colourFor('P')).toMatch(/^#/);
    expect(c.colourFor('P')).not.toBe(c.colourFor('D'));
  });

  it('a tap opens the line’s sheet, and a write there refreshes', async () => {
    const { c, changed, sheet, closed } = await setup([moment([said(1, 'P', { text: 'hallo' })])]);
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
    const { c, changed, closed } = await setup([moment([said(1, 'P')])]);
    c.openLine(c.turns()[0]);
    closed.next(undefined);
    expect(changed).not.toHaveBeenCalled();
  });

  it('a transcribed line is grey', async () => {
    const { c } = await setup([moment([said(1, 'P', { tier: 'transcribed' })])]);
    expect(c.provisional(c.turns()[0])).toBe(true);
  });

  it('plays a run as one span, pauses, then resumes in place', async () => {
    const { play, pause } = stubMedia();
    const { c, player } = await setup([moment([said(1, 'P'), said(2, 'P')])]);
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
    const { fixture, c, player } = await setup([moment([said(1, 'P')])]);
    const other = TestBed.createComponent(Turns);
    other.componentRef.setInput('moments', [moment([said(2, 'P')])]);
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
    const { c } = await setup([moment([usb], [phone])]);
    expect(c.disputed(usb)).toBe(true);
    expect(c.mics(usb)).toBe(2);
    expect(c.alternates(usb)).toEqual([phone]);
  });

  it('tags the mic where it changes, only when several are in view', async () => {
    const one = await setup([moment([said(1, 'P'), said(2, 'P')])]);
    expect(one.c.sourceTags().size).toBe(0);
    TestBed.resetTestingModule();
    const two = await setup([
      moment([said(1, 'P', { source: 'usb' })]),
      moment([said(2, 'P', { source: 'usb' })]),
      moment([said(3, 'P', { source: 'phone' })]),
    ]);
    expect([...two.c.sourceTags().entries()]).toEqual([
      [1, 'usb'],
      [3, 'phone'],
    ]);
  });
});
