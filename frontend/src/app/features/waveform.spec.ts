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

/** A sound two minutes into the span — the kind a delete has to account for. */
const SOUND = {
  start: new Date(Date.parse(SPAN_START) + 120_000).toISOString(),
  end: new Date(Date.parse(SPAN_START) + 121_000).toISOString(),
  peakDb: -41,
};

/** Three sounds whose loudness order is deliberately NOT their time order. */
function sound(atMinutes: number, peakDb: number) {
  const start = Date.parse(SPAN_START) + atMinutes * 60_000;
  return {
    start: new Date(start).toISOString(),
    end: new Date(start + 500).toISOString(),
    peakDb,
  };
}
const SCATTERED = [sound(1, -58), sound(4, -40), sound(7, -55)];

/** The server reports only what overlaps the requested window — segments and sounds. */
function envelopeFor(from: Date, to: Date): Envelope {
  const segments = Array.from({ length: 10 }, (_, i) => segment(i)).filter(
    (s) => Date.parse(s.start) < to.getTime() && Date.parse(s.end) > from.getTime(),
  );
  const events =
    Date.parse(SOUND.start) < to.getTime() && Date.parse(SOUND.end) > from.getTime()
      ? [SOUND]
      : [];
  return {
    start: from.toISOString(),
    end: to.toISOString(),
    bucketS: 1,
    thresholdDb: -60,
    points: [],
    segments,
    events,
  };
}

function setup(events?: typeof SCATTERED) {
  const quietEnvelope = vi.fn((_source: string, from: Date, to: Date) => {
    const envelope = envelopeFor(from, to);
    return of(events ? { ...envelope, events } : envelope);
  });
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      { provide: RecallApi, useValue: { quietEnvelope, quietAudioUrl: () => '' } },
    ],
  });
  const fixture = TestBed.createComponent(Waveform);
  // jsdom's canvas has no pointer-capture API; the gestures under test don't need it.
  const canvas = (fixture.nativeElement as HTMLElement).querySelector('canvas');
  Object.assign(canvas ?? {}, {
    setPointerCapture: () => undefined,
    releasePointerCapture: () => undefined,
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 1000, height: 132 }),
  });
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

  it('still reports the span\'s sounds when the view is panned off them', () => {
    // The dangerous version of the same bug: panned away, the sound list emptied and the
    // UI said "no sound at all in this span" — a reassurance about audio it was not even
    // looking at, in a view whose whole job is approving a deletion.
    const { c } = setup();
    vi.advanceTimersByTime(200);
    expect(c.events()).toHaveLength(1);

    c.viewFrom.set(Date.parse(SPAN_END));
    c.viewTo.set(Date.parse(SPAN_END) + 600_000);
    vi.advanceTimersByTime(200);

    expect(c.events()).toHaveLength(1); // the sound is still in what would be deleted
  });

  it('does not claim to be playing when the browser refuses to play', async () => {
    // The phone showed a lit Stop button with no sound coming out: `playing` was set the
    // moment playback was *asked for*, and the rejected promise was dropped. "I heard
    // nothing" and "it never played" then look identical — in a view used to approve
    // deleting audio, that is the worst possible failure.
    const { fixture, c } = setup();
    vi.advanceTimersByTime(200);

    const audio = (fixture.nativeElement as HTMLElement).querySelector('audio')!;
    const refused = new Error('NotAllowedError');
    vi.spyOn(audio, 'play').mockRejectedValue(refused);
    vi.spyOn(audio, 'load').mockImplementation(() => undefined);

    c.playSpan();
    audio.dispatchEvent(new Event('loadedmetadata')); // metadata arrives; play() is asked
    await vi.waitFor(() => expect(c.playing()).toBe(false));

    expect(c.playing()).toBe(false);
    expect(c.problem()).toContain('could not play');
  });

  it('reports it is playing only once sound is actually coming out', () => {
    const { fixture, c } = setup();
    vi.advanceTimersByTime(200);
    const audio = (fixture.nativeElement as HTMLElement).querySelector('audio')!;
    vi.spyOn(audio, 'play').mockResolvedValue(undefined);
    vi.spyOn(audio, 'load').mockImplementation(() => undefined);

    c.playSpan();
    expect(c.playing()).toBe(false); // asked, but nothing is out yet

    audio.dispatchEvent(new Event('playing')); // the element says sound is flowing
    expect(c.playing()).toBe(true);

    audio.dispatchEvent(new Event('pause'));
    expect(c.playing()).toBe(false);
  });

  function pointer(type: string, id: number, x: number): PointerEvent {
    return {
      pointerId: id,
      clientX: x,
      type,
      preventDefault: () => undefined,
    } as unknown as PointerEvent;
  }

  it('pinches to zoom, holding the time under the fingers still', () => {
    // Zoom was bound to the mouse wheel, which a phone does not have — so on the device
    // this review is actually used on, the waveform was stuck at one scale and a
    // half-second sound was a few pixels wide.
    const { c } = setup();
    vi.advanceTimersByTime(200);
    c.width.set(1000);
    const before = c.viewTo() - c.viewFrom();

    c.onPointerDown(pointer('pointerdown', 1, 400));
    c.onPointerDown(pointer('pointerdown', 2, 600)); // two fingers, 200px apart
    c.onPointerMove(pointer('pointermove', 1, 300));
    c.onPointerMove(pointer('pointermove', 2, 700)); // spread to 400px = 2x zoom in

    const after = c.viewTo() - c.viewFrom();
    expect(after).toBeCloseTo(before / 2, -2);
  });

  it('does not start playback when a pinch ends', () => {
    const { c } = setup();
    vi.advanceTimersByTime(200);
    const played = vi.spyOn(c, 'playFrom' as never);

    c.onPointerDown(pointer('pointerdown', 1, 400));
    c.onPointerDown(pointer('pointerdown', 2, 600));
    c.onPointerUp(pointer('pointerup', 2, 600));
    c.onPointerUp(pointer('pointerup', 1, 400)); // lifting out of a pinch is not a tap

    expect(played).not.toHaveBeenCalled();
  });

  it('fits the span back into view after panning away', () => {
    const { c } = setup();
    vi.advanceTimersByTime(200);
    const [from, to] = [c.viewFrom(), c.viewTo()];

    c.viewFrom.set(Date.parse(SPAN_END) + 3_600_000); // panned an hour past the span
    c.viewTo.set(Date.parse(SPAN_END) + 4_200_000);
    c.fitSpan();

    expect(c.viewFrom()).toBe(from);
    expect(c.viewTo()).toBe(to);
    expect(c.viewFrom()).toBeLessThan(Date.parse(SPAN_START));
    expect(c.viewTo()).toBeGreaterThan(Date.parse(SPAN_END));
  });

  it('steps through the sounds in time order, so next means later', () => {
    // They were once ordered loudest-first, so the arrows jumped around the timeline and
    // fought the picture they sit under. Forward must mean to the right.
    const { c } = setup(SCATTERED);
    vi.advanceTimersByTime(200);

    const times = c.events().map((e: { start: string }) => Date.parse(e.start));
    expect(times).toEqual([...times].sort((a, b) => a - b));

    c.cursor.set(0);
    c.step(1);
    expect(c.cursor()).toBe(1);
    expect(c.current().peakDb).toBe(-40); // the 4-minute one, not the loudest-by-rank
  });

  it('reaches the loudest sound in one press without reordering time', () => {
    // The triage question — is any of this speech? — is the loudest sound, and it should
    // cost one press rather than a walk through every crackle in the span.
    const { c } = setup(SCATTERED);
    vi.advanceTimersByTime(200);

    expect(c.loudest()).toBe(1); // second in time, loudest in level
    c.toLoudest();
    expect(c.cursor()).toBe(1);
    expect(c.onLoudest()).toBe(true);
    expect(c.current().peakDb).toBe(-40);
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
