import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideZonelessChangeDetection } from '@angular/core';
import { BehaviorSubject, of } from 'rxjs';
import { vi } from 'vitest';

import { App } from './app';
import { provideServiceWorker } from '@angular/service-worker';
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
  // A press answers mid-transition: desired flipped, confirmed unchanged, as
  // the next poll would.
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
      // The shell starts the update check; disabled, as with no worker.
      provideServiceWorker('ngsw-worker.js', { enabled: false }),
    ],
  });
  const fixture = TestBed.createComponent(App);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
  const c = fixture.componentInstance as any;
  return { fixture, c, capture, pauseCapture, resumeCapture };
}

describe('App', () => {
  it('creates the shell', () => {
    const { fixture } = setup();
    expect(fixture.componentInstance).toBeTruthy();
  });

  it('names the app in the bar and lists every main screen', async () => {
    const { fixture } = setup();
    await fixture.whenStable();
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('ui-scaffold h1')?.textContent).toContain('recall');
    // jsdom does not match the handset breakpoint, so the screens are in the rail.
    const nav = el.querySelector('nav[aria-label="Main screens"]');
    expect(nav?.querySelectorAll('a').length).toBe(4);
    const navText = nav?.textContent ?? '';
    for (const label of ['Timeline', 'Sessions', 'Check', 'Search']) {
      expect(navText).toContain(label);
    }
    // Removed screens: a tab outliving its route falls through to '' silently.
    expect(navText).not.toContain('Ask');
    expect(navText).not.toContain('Train');
  });

  it('the menu holds Labels and the build stamp', async () => {
    const { fixture } = setup();
    await fixture.whenStable();
    const el = fixture.nativeElement as HTMLElement;
    el.querySelector<HTMLButtonElement>('ui-scaffold button[aria-label="Menu"]')?.click();
    await fixture.whenStable();
    // mat-menu renders into the CDK overlay, outside the component element.
    const overlay = document.querySelector('.cdk-overlay-container');
    const texts = [...(overlay?.querySelectorAll('[mat-menu-item]') ?? [])].map(
      (i) => i.textContent ?? '',
    );
    expect(texts.some((t) => t.includes('Labels'))).toBe(true);
    expect(texts.some((t) => t.includes('Compare'))).toBe(false);
    expect(overlay?.querySelector('.version')?.textContent).toContain(BUILD_INFO.sha);
  });

  it('shows the paused banner only when capture is paused', () => {
    const { fixture, c } = setup(cap({ running: false, pausedUntil: '2026-06-17T20:00:00Z' }));
    fixture.detectChanges();
    expect(c.paused()).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.paused-banner')).toBeTruthy();
  });

  it('resume-by leads with a yyyy-mm-dd date before the time', () => {
    const { c } = setup(cap({ running: false, pausedUntil: '2026-06-17T20:00:00Z' }));
    // The shape only: the value depends on the runner's timezone.
    expect(c.resumeBy()).toMatch(/^\d{4}-\d{2}-\d{2} \S/);
  });

  it('resume-in shows the remaining hours/minutes', () => {
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
    // Once, the press said paused and the next poll the mic's stale running,
    // and the banner blinked between them.
    const { fixture, c, pauseCapture } = setup();
    expect(c.paused()).toBe(false);
    c.pauseCapture();
    expect(pauseCapture).toHaveBeenCalled();
    expect(c.paused()).toBe(true);
    expect(c.transitioning()).toBe(true);
    expect(c.transitionLabel()).toBe('Pausing');
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.paused-banner.transitioning')?.textContent).toContain('Pausing');
    // Not yet the settled banner with its resume buttons.
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
    // Pressing the opposite mid-transition just replaces the desired state.
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
