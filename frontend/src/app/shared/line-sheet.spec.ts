import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
import { MAT_BOTTOM_SHEET_DATA, MatBottomSheetRef } from '@angular/material/bottom-sheet';
import { MatSnackBar } from '@angular/material/snack-bar';
import { vi } from 'vitest';

import { LineSheet, LineSheetData, wordsOf } from './line-sheet';
import { Transcript } from '../models';

function turn(o: Partial<Transcript>): Transcript {
  return {
    id: 7,
    start: '2026-01-15T10:33:00Z',
    end: '2026-01-15T10:33:05Z',
    text: 'a list of errands',
    language: 'en',
    speaker: 'P',
    speakerConfirmed: true,
    speakerConfidence: null,
    confidence: null,
    loudness: null,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    audioUrl: '/api/audio/7',
    source: 'usb',
    cluster: null,
    ...o,
  };
}

async function setup(o: Partial<Transcript> = {}, alternates: Transcript[] = []) {
  const open = vi.fn();
  const dismiss = vi.fn();
  const data: LineSheetData = {
    turn: turn(o),
    speaker: 'P',
    confirmed: true,
    names: ['P', 'D'],
    known: ['P', 'D', 'Sam'],
    alternates,
  };
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      provideRouter([]),
      { provide: MatSnackBar, useValue: { open } },
      { provide: MAT_BOTTOM_SHEET_DATA, useValue: data },
      { provide: MatBottomSheetRef, useValue: { dismiss } },
    ],
  });
  const fixture = TestBed.createComponent(LineSheet);
  fixture.detectChanges();
  await fixture.whenStable();
  const ctrl = TestBed.inject(HttpTestingController);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
  const c = fixture.componentInstance as any;
  const el = fixture.nativeElement as HTMLElement;
  return { fixture, c, el, ctrl, open, dismiss };
}

describe('wordsOf', () => {
  it('gives character offsets, end exclusive, across extra spaces', () => {
    expect(wordsOf(' ab  cd ')).toEqual([
      { text: 'ab', start: 1, end: 3 },
      { text: 'cd', start: 5, end: 7 },
    ]);
  });

  it('counts a character outside the BMP as one, as the server does', () => {
    expect(wordsOf('😀 ok')[1]).toEqual({ text: 'ok', start: 2, end: 4 });
  });
});

describe('LineSheet', () => {
  it('saying who files a correction that keeps the words, and closes', async () => {
    const { c, ctrl, dismiss } = await setup();
    c.choose('D');
    const req = ctrl.expectOne('/api/correct');
    expect(req.request.body).toEqual({ id: 7, text: 'a list of errands', speaker: 'D' });
    req.flush({ newId: 9 });
    expect(dismiss).toHaveBeenCalledWith('wrote');
  });

  it('re-picking the confirmed speaker writes nothing', async () => {
    const { c, ctrl } = await setup();
    c.choose('P');
    ctrl.expectNone('/api/correct');
  });

  it('a failed write says so and stays open', async () => {
    const { c, ctrl, open, dismiss } = await setup();
    c.choose('D');
    ctrl.expectOne('/api/correct').flush('no', { status: 500, statusText: 'x' });
    expect(open).toHaveBeenCalled();
    expect(dismiss).not.toHaveBeenCalled();
    expect(c.busy()).toBe(false);
  });

  it('a live line offers no edits', async () => {
    const { el } = await setup({ tier: 'live' });
    expect(el.textContent).toContain('Still being transcribed');
    expect(el.textContent).not.toContain('Fix words');
  });

  it('fixing the words saves the new text; unchanged or blank posts nothing', async () => {
    const { c, ctrl } = await setup({ text: 'foracidinib' });
    c.saveText('  ');
    c.saveText('foracidinib');
    ctrl.expectNone('/api/correct');
    c.saveText('vorasidenib');
    const req = ctrl.expectOne('/api/correct');
    expect(req.request.body).toEqual({ id: 7, text: 'vorasidenib' });
  });

  it('two taps pick a range of words, a third starts over', async () => {
    const { c } = await setup({ text: 'one two three four' });
    c.open('part');
    c.pick(2);
    c.pick(1);
    expect(c.partText()).toBe('two three');
    c.pick(3);
    expect(c.partText()).toBe('four');
  });

  it('gives the picked words to someone, once however often tapped', async () => {
    const { c, ctrl, dismiss } = await setup();
    c.open('part');
    c.pick(0);
    c.pick(1);
    c.choose('Sam');
    c.choose('Sam');
    const reqs = ctrl.match('/api/sessions/usb/assign');
    expect(reqs.length).toBe(1);
    expect(reqs[0].request.body).toEqual({
      startTurn: 7,
      startChar: 0,
      endTurn: 7,
      endChar: 6,
      name: 'Sam',
    });
    reqs[0].flush({ touched: 1 });
    expect(dismiss).toHaveBeenCalledWith('wrote');
  });

  it('a single picked word is a part too', async () => {
    const { c, ctrl } = await setup();
    c.open('part');
    c.pick(3);
    c.choose('D');
    const body = ctrl.expectOne('/api/sessions/usb/assign').request.body;
    expect([body.startChar, body.endChar]).toEqual([10, 17]);
  });
});
