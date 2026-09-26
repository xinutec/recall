import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { provideRouter } from '@angular/router';
import { MatSnackBar } from '@angular/material/snack-bar';
import { vi } from 'vitest';

import { Check, dayWindow, otherMics, pickLines } from './check';
import { Moment, Transcript } from '../models';

function line(id: number, source: string, o: Partial<Transcript> = {}): Transcript {
  return {
    id,
    start: '2026-09-19T10:00:00+00:00',
    end: '2026-09-19T10:00:04+00:00',
    text: `line ${id}`,
    language: 'en',
    speaker: null,
    speakerConfirmed: false,
    speakerConfidence: null,
    confidence: null,
    loudness: null,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    audioUrl: `/api/audio/${id}`,
    source,
    cluster: null,
    ...o,
  };
}

const moment = (primary: Transcript, ...alternates: Transcript[]): Moment => ({
  start: primary.start,
  end: primary.end,
  primary: [primary],
  alternates,
  sources: [primary, ...alternates].map((t) => t.source ?? ''),
});

describe('pickLines', () => {
  it('spreads the checks across the mics that heard each moment', () => {
    const picked = pickLines([
      moment(line(1, 'usb'), line(2, 'pixel9')),
      moment(line(3, 'usb'), line(4, 'pixel9')),
      moment(line(5, 'usb'), line(6, 'pixel9')),
      moment(line(7, 'usb'), line(8, 'pixel9')),
    ]);
    expect(picked.map((t) => t.source)).toEqual(['usb', 'pixel9', 'usb', 'pixel9']);
  });

  it('skips a moment already corrected, a live line, and a blank one', () => {
    const picked = pickLines([
      moment(line(1, 'usb', { tier: 'corrected' }), line(2, 'pixel9')),
      moment(line(3, 'usb', { tier: 'live' })),
      moment(line(4, 'usb', { text: '  ' })),
      moment(line(5, 'usb', { tier: 'transcribed' })),
    ]);
    expect(picked.map((t) => t.id)).toEqual([5]);
  });
});

describe('otherMics', () => {
  it('gives each line the same moment as the other mics heard it', () => {
    const others = otherMics([
      moment(line(1, 'usb'), line(2, 'geb'), line(3, 'pixel9', { tier: 'live' })),
    ]);
    expect(others.get(1)?.map((t) => t.id)).toEqual([2]);
    expect(others.get(2)?.map((t) => t.id)).toEqual([1]);
  });
});

describe('dayWindow', () => {
  it('is the local day, in the stored spelling', () => {
    const { after, before } = dayWindow('2026-09-19');
    expect(new Date(after).getTime()).toBe(new Date(2026, 8, 19).getTime());
    expect(new Date(before).getTime()).toBe(new Date(2026, 8, 20).getTime());
    expect(after).toMatch(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\+00:00$/);
  });
});

describe('Check', () => {
  afterEach(() => vi.restoreAllMocks());

  async function setup(ids: string) {
    vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue(undefined);
    vi.spyOn(HTMLMediaElement.prototype, 'pause').mockReturnValue(undefined);
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
    const fixture = TestBed.createComponent(Check);
    fixture.componentRef.setInput('ids', ids);
    fixture.detectChanges();
    const ctrl = TestBed.inject(HttpTestingController);
    ctrl
      .expectOne('/api/transcripts?ids=1%2C2')
      .flush({ items: [line(1, 'usb'), line(2, 'usb')] });
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
    const c = fixture.componentInstance as any;
    return { c, ctrl, open };
  }

  it('files unchanged words as checked, then moves on', async () => {
    const { c, ctrl } = await setup('1,2');
    expect(c.current().id).toBe(1);
    c.confirm();
    const req = ctrl.expectOne('/api/correct');
    expect(req.request.body).toEqual({ id: 1, text: 'line 1', checked: true });
    req.flush({ newId: 9 });
    expect(c.current().id).toBe(2);
    expect(c.checked().get(1)).toBe('line 1');
  });

  it('files fixed words, and a checked line cannot be filed twice', async () => {
    const { c, ctrl } = await setup('1,2');
    c.draft.set('the right words');
    expect(c.dirty()).toBe(true);
    c.confirm();
    ctrl.expectOne('/api/correct').flush({ newId: 9 });
    c.back();
    c.confirm();
    ctrl.expectNone('/api/correct');
  });

  it('a failed save says so and stays on the line', async () => {
    const { c, ctrl, open } = await setup('1,2');
    c.confirm();
    ctrl.expectOne('/api/correct').flush('no', { status: 500, statusText: 'x' });
    expect(open).toHaveBeenCalled();
    expect(c.current().id).toBe(1);
  });
});
