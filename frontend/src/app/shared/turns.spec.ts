import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { vi } from 'vitest';

import { Turns, runsOf } from './turns';
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
  const open = vi.fn();
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      provideRouter([]),
      { provide: MatSnackBar, useValue: { open } },
    ],
  });
  const fixture = TestBed.createComponent(Turns);
  fixture.componentRef.setInput('moments', moments);
  fixture.componentRef.setInput('roster', roster);
  const changed = vi.fn();
  fixture.componentInstance.changed.subscribe(changed);
  fixture.detectChanges();
  await fixture.whenStable();
  const ctrl = TestBed.inject(HttpTestingController);
  const player = TestBed.inject(Player);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
  const c = fixture.componentInstance as any;
  return { fixture, c, ctrl, changed, open, player };
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

  it('a tap selects a line, and a second tap clears it', async () => {
    const { c } = await setup([moment([said(1, 'P')])]);
    c.selectTurn(1);
    expect(c.selected()).toBe(1);
    c.selectTurn(1);
    expect(c.selected()).toBeNull();
  });

  it('saying who files a correction that keeps the words', async () => {
    const { c, ctrl, changed } = await setup([moment([said(1, 'P', { text: 'hallo' })])]);
    c.selectTurn(1);
    c.assignTurn('D');
    const req = ctrl.expectOne('/api/correct');
    expect(req.request.body).toEqual({ id: 1, text: 'hallo', speaker: 'D' });
    req.flush({ newId: 9 });
    expect(c.selected()).toBeNull();
    expect(changed).toHaveBeenCalled();
  });

  it('a failed write says so and emits nothing', async () => {
    const { c, ctrl, changed, open } = await setup([moment([said(1, 'P')])]);
    c.selectTurn(1);
    c.assignTurn('D');
    ctrl.expectOne('/api/correct').flush('no', { status: 500, statusText: 'x' });
    expect(open).toHaveBeenCalled();
    expect(changed).not.toHaveBeenCalled();
  });

  it('a transcribed line is editable, a live one is not', async () => {
    const { c } = await setup([
      moment([said(1, 'P', { tier: 'transcribed' })]),
      moment([said(2, 'P', { tier: 'live' })]),
    ]);
    const [transcribed, live] = c.turns();
    expect(c.editable(transcribed)).toBe(true);
    expect(c.provisional(transcribed)).toBe(true);
    expect(c.editable(live)).toBe(false);
  });

  it('Fix words opens on the tapped line and saves the new text', async () => {
    const { c, ctrl } = await setup([moment([said(1, 'P', { text: 'foracidinib' })])]);
    c.selectTurn(1);
    c.editText();
    expect(c.editingText()).toBe('foracidinib');
    c.saveEdit('vorasidenib');
    const req = ctrl.expectOne('/api/correct');
    expect(req.request.body).toEqual({ id: 1, text: 'vorasidenib' });
    req.flush({ newId: 99 });
    expect(c.editing()).toBeNull();
  });

  it('a blank edit posts nothing', async () => {
    const { c, ctrl } = await setup([moment([said(1, 'P')])]);
    c.editing.set(1);
    c.saveEdit('   ');
    ctrl.expectNone('/api/correct');
  });

  it('maps a selection to turn and offsets, clamping the trailing space', async () => {
    const text = 'a list of errands';
    const { fixture, c } = await setup([moment([said(7, 'P', { text })])]);
    const node = fixture.nativeElement.querySelector('span.t[data-id="7"]').firstChild as Text;
    const range = document.createRange();
    range.setStart(node, 0);
    range.setEnd(node, text.length + 1);
    vi.spyOn(window, 'getSelection').mockReturnValue({
      isCollapsed: false,
      rangeCount: 1,
      getRangeAt: () => range,
      toString: () => text,
    } as unknown as Selection);
    c.onSelect();
    expect(c.span()).toEqual({ startTurn: 7, startChar: 0, endTurn: 7, endChar: text.length });
    expect(c.spanSource()).toBe('m');
  });

  it('moves a selected phrase to the turn’s source, once however often tapped', async () => {
    const { c, ctrl } = await setup([moment([said(7, 'P')])]);
    c.span.set({ startTurn: 7, startChar: 0, endTurn: 7, endChar: 5 });
    c.spanSource.set('usb');
    c.assignSpan('D');
    c.assignSpan('D');
    const reqs = ctrl.match('/api/sessions/usb/assign');
    expect(reqs.length).toBe(1);
    expect(reqs[0].request.body).toEqual({
      startTurn: 7,
      startChar: 0,
      endTurn: 7,
      endChar: 5,
      name: 'D',
    });
    reqs[0].flush({ touched: 1 });
    expect(c.span()).toBeNull();
  });

  it('does not move a phrase without a name or a source', async () => {
    const { c, ctrl } = await setup([moment([said(7, 'P')])]);
    c.span.set({ startTurn: 7, startChar: 0, endTurn: 7, endChar: 5 });
    c.spanSource.set('usb');
    c.assignSpan('   ');
    c.spanSource.set(null);
    c.assignSpan('D');
    ctrl.expectNone(() => true);
    expect(c.span()).not.toBeNull();
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
