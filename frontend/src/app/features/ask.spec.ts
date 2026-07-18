import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { Subject, of } from 'rxjs';
import { vi } from 'vitest';

import { Ask } from './ask';
import { RecallApi } from '../recall-api';
import { AskAnswer, Transcript } from '../models';

const TODAY_FRESH = {
  day: '2026-06-13',
  text: null as string | null,
  generatedAt: '2026-06-13T10:00:00Z',
  upToDate: true,
  pending: false,
};

const TURN: Transcript = {
  id: 7,
  start: '2026-06-13T12:00:00Z',
  end: '2026-06-13T12:00:02Z',
  text: 'the plumber is coming on Thursday',
  language: 'en',
  speaker: 'Alice',
  speakerConfirmed: true,
  speakerConfidence: null,
  confidence: 0.9,
  loudness: 0.05,
  model: 'whisper',
  tier: 'transcribed',
  hidden: null,
  audioUrl: '/api/audio/7',
  source: 'usb',
  cluster: null,
};

function askFn(result: () => AskAnswer | Subject<AskAnswer>) {
  return vi.fn((q: string) => {
    void q;
    const r = result();
    return r instanceof Subject ? r : of(r);
  });
}

// A resolved answer (the Mac-inline path, or a poll that has landed).
function done(answer: string | null, sources: readonly Transcript[] = []): AskAnswer {
  return { status: 'done', id: null, answer, sources, error: null };
}

function setup(
  ask = askFn(() => done('x')),
  askStatus = vi.fn((id: number) => {
    void id;
    return of(done('x'));
  }),
) {
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: RecallApi, useValue: { ask, askStatus } },
    ],
  });
  const fixture = TestBed.createComponent(Ask);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  const http = TestBed.inject(HttpTestingController);
  return { fixture, c, ask, askStatus, http };
}

describe('Ask', () => {
  it('submits the question and renders the cited answer', () => {
    const ask = askFn(() => done('Thursday, per Alice.', [TURN]));
    const { fixture, c } = setup(ask);
    c.question.set('When is the plumber coming?');
    c.submit();
    fixture.detectChanges(); // of() emits synchronously; render the result
    const el = fixture.nativeElement as HTMLElement;
    expect(ask).toHaveBeenCalledWith('When is the plumber coming?');
    expect(el.textContent).toContain('Thursday, per Alice.');
    expect(el.textContent).toContain('the plumber is coming on Thursday');
  });

  it('renders the honest no-evidence message for a null answer', () => {
    const ask = askFn(() => done(null));
    const { fixture, c } = setup(ask);
    c.question.set('zeppelins?');
    c.submit();
    fixture.detectChanges();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain(
      "don't show anything",
    );
  });

  it('shows progress while the local generation runs and blocks double-submit', () => {
    const pending = new Subject<AskAnswer>();
    const ask = askFn(() => pending);
    const { c } = setup(ask);
    c.question.set('anything');
    c.submit();
    expect(c.asking()).toBe(true);
    c.submit(); // second tap while in flight
    expect(ask).toHaveBeenCalledTimes(1);
    pending.next(done('done'));
    pending.complete();
    expect(c.asking()).toBe(false);
  });

  it('polls a pending ask until the Mac lands the answer', async () => {
    vi.useFakeTimers();
    try {
      // The fleet path: POST returns 'pending' with a poll id and the retrieved sources;
      // the component shows them and polls askStatus until the answer arrives.
      const ask = askFn(() => ({
        status: 'pending' as const,
        id: 5,
        answer: null,
        sources: [TURN],
        error: null,
      }));
      const askStatus = vi.fn((id: number) => {
        void id;
        return of(done('Thursday, per Alice.', [TURN]));
      });
      const { fixture, c } = setup(ask, askStatus);
      c.question.set('when?');
      c.submit();
      fixture.detectChanges();
      expect(c.asking()).toBe(true); // still waiting…
      expect((fixture.nativeElement as HTMLElement).textContent).toContain(
        'Found these', // sources shown while it generates
      );

      await vi.advanceTimersByTimeAsync(2500); // ASK_POLL_MS → the poll fires
      fixture.detectChanges();
      expect(askStatus).toHaveBeenCalledWith(5);
      expect(c.asking()).toBe(false);
      expect((fixture.nativeElement as HTMLElement).textContent).toContain(
        'Thursday, per Alice.',
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it('surfaces a generation error reported by the poll', async () => {
    vi.useFakeTimers();
    try {
      const ask = askFn(() => ({
        status: 'pending' as const,
        id: 8,
        answer: null,
        sources: [],
        error: null,
      }));
      const askStatus = vi.fn((id: number) => {
        void id;
        return of<AskAnswer>({
          status: 'error',
          id: null,
          answer: null,
          sources: [],
          error: 'model failed to load',
        });
      });
      const { fixture, c } = setup(ask, askStatus);
      c.question.set('when?');
      c.submit();
      await vi.advanceTimersByTimeAsync(2500);
      fixture.detectChanges();
      expect(c.asking()).toBe(false);
      expect((fixture.nativeElement as HTMLElement).textContent).toContain(
        'model failed to load',
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it('lists the recent day summaries', async () => {
    const { fixture, http } = setup();
    fixture.detectChanges(); // let the httpResource issue its request
    await Promise.resolve(); // resource requests are scheduled on a microtask
    // Both page resources fire; flush both or whenStable() never settles.
    http.expectOne('/api/summaries/today').flush(TODAY_FRESH);
    http
      .expectOne('/api/summaries')
      .flush({ items: [{ day: '2026-06-13', text: 'Plans were made.', model: 'm' }] });
    await fixture.whenStable();
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Plans were made.');
  });

  it('shows the today-so-far summary with its as-of time', async () => {
    const { fixture, http } = setup();
    fixture.detectChanges();
    await Promise.resolve();
    http.expectOne('/api/summaries').flush({ items: [] });
    http.expectOne('/api/summaries/today').flush({
      ...TODAY_FRESH,
      text: 'So far: plumber Thursday.',
    });
    await fixture.whenStable();
    const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(text).toContain('So far: plumber Thursday.');
    expect(text).toContain('as of');
    expect(text).not.toContain('updating');
  });

  it('marks a stale summary as updating and re-polls until fresh', async () => {
    vi.useFakeTimers();
    try {
      const { fixture, http } = setup();
      fixture.detectChanges();
      await Promise.resolve();
      http.expectOne('/api/summaries').flush({ items: [] });
      http.expectOne('/api/summaries/today').flush({
        ...TODAY_FRESH,
        text: 'Morning only.',
        upToDate: false,
        pending: true,
      });
      // Response delivery is itself scheduled — let it run under fake timers.
      await vi.advanceTimersByTimeAsync(1);
      fixture.detectChanges();
      const text = (fixture.nativeElement as HTMLElement).textContent ?? '';
      expect(text).toContain('Morning only.'); // stale text still shown...
      expect(text).toContain('updating'); // ...but marked as being refreshed

      await vi.advanceTimersByTimeAsync(8000); // past TODAY_POLL_MS → re-poll fires
      http.expectOne('/api/summaries/today'); // the re-poll went out
    } finally {
      vi.useRealTimers();
    }
  });
});
