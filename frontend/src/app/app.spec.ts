import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideZonelessChangeDetection } from '@angular/core';
import { BehaviorSubject, of } from 'rxjs';
import { vi } from 'vitest';

import { App } from './app';
import { BUILD_INFO } from './build-info';
import { RecallApi } from './recall-api';
import { CaptureState } from './models';

/** A settled CaptureState (desired == confirmed), overridable per test. */
function cap(overrides: Partial<CaptureState> = {}): CaptureState {
  const running = overrides.running ?? true;
  const pausedUntil = overrides.pausedUntil ?? null;
  return {
    running,
    pausedUntil,
    desiredRunning: running,
    desiredPausedUntil: pausedUntil,
    settled: true,
    micReachable: true,
    stateToken: '',
    ...overrides,
  };
}

function setup(initial: CaptureState = cap()) {
  const state = new BehaviorSubject<CaptureState>(initial);
  const capture = vi.fn(() => state);
  // A press answers with the TRANSITIONING shape: desired flipped, confirmed
  // unchanged — the same shape the next poll returns, so nothing can flap.
  const pauseCapture = vi.fn(() =>
    of(
      cap({
        running: true,
        desiredRunning: false,
        desiredPausedUntil: '2026-06-17T20:00:00Z',
        settled: false,
      }),
    ),
  );
  const resumeCapture = vi.fn(() =>
    of(
      cap({
        running: false,
        pausedUntil: '2026-06-17T20:00:00Z',
        desiredRunning: true,
        desiredPausedUntil: null,
        settled: false,
      }),
    ),
  );
  TestBed.configureTestingModule({
    imports: [App],
    providers: [
      provideZonelessChangeDetection(),
      provideRouter([]),
      { provide: RecallApi, useValue: { capture, pauseCapture, resumeCapture } },
    ],
  });
  const fixture = TestBed.createComponent(App);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  return { fixture, c, capture, pauseCapture, resumeCapture };
}

describe('App', () => {
  it('creates the shell', () => {
    const { fixture } = setup();
    expect(fixture.componentInstance).toBeTruthy();
  });

  it('renders the brand and every primary nav link', async () => {
    const { fixture } = setup();
    await fixture.whenStable();
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.brand')?.textContent).toContain('recall');
    const links = [...el.querySelectorAll('.links a')];
    expect(links.length).toBe(5);
    const navText = el.querySelector('.links')?.textContent ?? '';
    for (const label of ['Timeline', 'Sessions', 'Train', 'Search', 'Ask']) {
      expect(navText).toContain(label);
    }
    // Secondary pages live in the hamburger menu, not the nav.
    expect(navText).not.toContain('Compare');
  });

  it('hamburger menu holds the secondary pages (Compare, Labels)', async () => {
    const { fixture } = setup();
    await fixture.whenStable();
    const el = fixture.nativeElement as HTMLElement;
    const ham = el.querySelector<HTMLButtonElement>('.ham-btn');
    expect(ham).toBeTruthy();
    ham?.click();
    await fixture.whenStable();
    // mat-menu renders into the CDK overlay, outside the component element.
    const items = [...document.querySelectorAll('.cdk-overlay-container [mat-menu-item]')];
    const texts = items.map((i) => i.textContent ?? '');
    expect(texts.some((t) => t.includes('Compare'))).toBe(true);
    expect(texts.some((t) => t.includes('Labels'))).toBe(true);
  });

  it('shows the build stamp so a stale cache is visible at a glance', async () => {
    const { fixture } = setup();
    await fixture.whenStable();
    const footer = (fixture.nativeElement as HTMLElement).querySelector('.version');
    expect(footer?.textContent).toContain(BUILD_INFO.sha);
  });

  it('shows the paused banner only when capture is paused', () => {
    const { fixture, c } = setup(cap({ running: false, pausedUntil: '2026-06-17T20:00:00Z' }));
    fixture.detectChanges();
    expect(c.paused()).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.paused-banner')).toBeTruthy();
  });

  it('resume-by leads with a yyyy-mm-dd date before the time', () => {
    const { c } = setup(cap({ running: false, pausedUntil: '2026-06-17T20:00:00Z' }));
    // Shape, not exact value — the local date/time depend on the runner's timezone.
    expect(c.resumeBy()).toMatch(/^\d{4}-\d{2}-\d{2} \S/);
  });

  it('resume-in shows the remaining hours/minutes', () => {
    // Timezone-independent (a duration, not a wall clock): a far-future deadline.
    const { c } = setup(
      cap({
        running: false,
        pausedUntil: new Date(Date.now() + 5 * 3_600_000 + 23 * 60_000).toISOString(),
      }),
    );
    expect(c.resumeIn()).toBe('5h 23m');
  });

  it('resume-in is empty when capture is running', () => {
    expect(setup().c.resumeIn()).toBe('');
  });

  it('a press flips to the desired state as transitioning — no flap possible', () => {
    // The flap seen live 2026-07-16: POST said "paused" (intent), the next poll
    // said "running" (the mic's stale report), and the banner blinked. Desired
    // and confirmed are now separate fields, rendered as an explicit "Pausing…".
    const { fixture, c, pauseCapture } = setup();
    expect(c.paused()).toBe(false);
    c.pauseCapture();
    expect(pauseCapture).toHaveBeenCalled();
    expect(c.paused()).toBe(true); // the banner follows the desired state…
    expect(c.transitioning()).toBe(true); // …flagged as awaiting confirmation
    expect(c.transitionLabel()).toBe('Pausing');
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.paused-banner.transitioning')?.textContent).toContain('Pausing');
    // the settled paused banner (with its resume buttons) is NOT shown yet
    expect(el.querySelector('.paused-banner .rec-dot')).toBeFalsy();
  });

  it('settles once the mic confirms: transitioning clears, paused banner shows', () => {
    const { fixture, c } = setup(cap({ running: false, pausedUntil: '2026-06-17T20:00:00Z' }));
    fixture.detectChanges();
    expect(c.transitioning()).toBe(false);
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.paused-banner .rec-dot')).toBeTruthy();
  });

  it('a transition is abortable: the toggle stays enabled to change your mind', () => {
    // Intent is cheap and idempotent — pressing the opposite action mid-transition
    // just overwrites the desired state. Freezing the buttons was the old flap
    // fix overshooting; only the LABEL needed to be honest.
    const { fixture, c } = setup();
    c.pauseCapture();
    expect(c.transitioning()).toBe(true);
    fixture.detectChanges();
    const btn = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '.capture-toggle',
    );
    expect(btn?.disabled).toBe(false);
  });

  it('an unreachable mic is said out loud, not presented as fact', () => {
    const { fixture, c } = setup(
      cap({ running: false, desiredRunning: false, settled: false, micReachable: false }),
    );
    fixture.detectChanges();
    expect(c.unreachable()).toBe(true);
    expect(c.transitioning()).toBe(false); // unknown ≠ in-flight
    const banner = (fixture.nativeElement as HTMLElement).querySelector(
      '.paused-banner.unreachable',
    );
    expect(banner?.textContent).toContain('not reporting');
  });
});
