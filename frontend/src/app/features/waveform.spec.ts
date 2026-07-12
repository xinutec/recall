import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { of } from 'rxjs';
import { afterEach, beforeEach, vi } from 'vitest';

import { Waveform } from './waveform';
import { RecallApi } from '../recall-api';
import { Envelope, EnvelopeSegment } from '../models';

const SPAN_START = '2026-06-13T22:19:00Z';
const SPAN_END = '2026-06-13T22:29:00Z';

beforeEach(() => {
  // jsdom has neither; the waveform observes its canvas for resizes and draws on a frame.
  // Neither matters to what is under test here — which segments a delete would take.
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe(): void {}
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

/** One minute-long capture segment, as the envelope reports it. */
function segment(index: number): EnvelopeSegment {
  const start = Date.parse(SPAN_START) + index * 60_000;
  return {
    audioId: 100 + index,
    start: new Date(start).toISOString(),
    end: new Date(start + 60_000).toISOString(),
    meanDb: -62,
  };
}

/** The server returns the segments overlapping the requested window — and only those. */
function envelopeFor(from: Date, to: Date): Envelope {
  const segments = Array.from({ length: 10 }, (_, i) => segment(i)).filter(
    (s) => Date.parse(s.start) < to.getTime() && Date.parse(s.end) > from.getTime(),
  );
  return {
    start: from.toISOString(),
    end: to.toISOString(),
    bucketS: 1,
    thresholdDb: -60,
    points: [],
    segments,
    events: [],
  };
}

function setup() {
  const quietEnvelope = vi.fn((_source: string, from: Date, to: Date) =>
    of(envelopeFor(from, to)),
  );
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      { provide: RecallApi, useValue: { quietEnvelope, quietAudioUrl: () => '' } },
    ],
  });
  const fixture = TestBed.createComponent(Waveform);
  fixture.componentRef.setInput('source', 'usb');
  fixture.componentRef.setInput('spanStart', SPAN_START);
  fixture.componentRef.setInput('spanEnd', SPAN_END);
  fixture.componentRef.setInput('selectedIds', []);
  fixture.detectChanges();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return { fixture, c, quietEnvelope };
}

describe('Waveform', () => {
  it('selects the whole span once its envelope has loaded', () => {
    const { c } = setup();
    vi.advanceTimersByTime(200); // the fetch is debounced
    expect(c.selectedIds()).toEqual(
      Array.from({ length: 10 }, (_, i) => 100 + i),
    );
  });

  it('does not change what a delete would take when the view is panned away', () => {
    // The bug this guards: the selection was derived from the segments in the *visible*
    // window, so dragging the waveform off the start of the span silently dropped those
    // segments — the delete count fell from 100 to 85 just by looking somewhere else.
    // Where you are looking is not what you are deleting.
    const { c } = setup();
    vi.advanceTimersByTime(200);
    const whole = c.selectedIds();
    expect(whole).toHaveLength(10);

    // Pan hard right: the window no longer covers the first half of the span.
    c.viewFrom.set(Date.parse(SPAN_END));
    c.viewTo.set(Date.parse(SPAN_END) + 600_000);
    vi.advanceTimersByTime(200);

    expect(c.selectedIds()).toEqual(whole);
  });

  it('narrows the selection only when an edge is trimmed', () => {
    const { c } = setup();
    vi.advanceTimersByTime(200);

    // Trim the first two minutes off the front.
    c.selFrom.set(Date.parse(SPAN_START) + 120_000);
    c.syncSelection();

    expect(c.selectedIds()).toEqual([102, 103, 104, 105, 106, 107, 108, 109]);
  });
});
