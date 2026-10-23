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

const END = '2026-06-17T20:00:00Z';

/** A settled CaptureState (desired == confirmed): off unless overridden. */
function cap(overrides: Partial<CaptureState> = {}): CaptureState {
  const until = overrides.until ?? null;
  const held = overrides.held ?? false;
  const running = overrides.running ?? (until !== null && !held);
  return {
    running,
    until,
    held,
    desiredRunning: running,
    desiredUntil: until,
    desiredHeld: held,
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
  const recordCapture = vi.fn((until: Date) =>
    of(
      cap({
        running: false,
        desiredRunning: true,
        desiredUntil: until.toISOString(),
        settled: false,
      }),
    ),
  );
  const answer = (overrides: Partial<CaptureState>) => vi.fn(() => of(cap(overrides)));
  const stopCapture = answer({});
  const pauseCapture = answer({ until: END, held: true });
  const unpauseCapture = answer({ until: END });
  TestBed.configureTestingModule({
    imports: [App],
    providers: [
      provideZonelessChangeDetection(),
      provideRouter([]),
      {
        provide: RecallApi,
        useValue: { capture, recordCapture, stopCapture, pauseCapture, unpauseCapture },
      },
      // The shell starts the update check; disabled, as with no worker.
      provideServiceWorker('ngsw-worker.js', { enabled: false }),
    ],
  });
  const fixture = TestBed.createComponent(App);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- the component's private state, reached in a test
  const c = fixture.componentInstance as any;
  return { fixture, c, capture, recordCapture, stopCapture, pauseCapture, unpauseCapture };
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

  it('off is the quiet default: no banner', () => {
    const { fixture, c } = setup();
    fixture.detectChanges();
    expect(c.mode()).toBe('off');
    expect((fixture.nativeElement as HTMLElement).querySelector('.capture-banner')).toBeFalsy();
  });

  it('an open window shows its end and the time left, with pause and stop', () => {
    const until = new Date(Date.now() + 45 * 60_000 + 20_000).toISOString();
    const { fixture, c } = setup(cap({ until }));
    fixture.detectChanges();
    expect(c.mode()).toBe('recording');
    expect(c.endsIn()).toBe('45m');
    const banner = (fixture.nativeElement as HTMLElement).querySelector('.capture-banner');
    expect(banner?.textContent).toContain('Recording until');
    expect(banner?.querySelector('[aria-label="Pause"]')).toBeTruthy();
    expect(banner?.querySelector('[aria-label="Stop"]')).toBeTruthy();
  });

  it('a window on another day names its date', () => {
    const { c } = setup(cap({ until: END }));
    // The shape only: the value depends on the runner's timezone.
    expect(c.endsAt()).toMatch(/^\d{4}-\d{2}-\d{2} \S/);
  });

  it('a paused window offers unpause, and keeps its end', () => {
    const { fixture, c } = setup(cap({ until: END, held: true }));
    fixture.detectChanges();
    expect(c.mode()).toBe('held');
    const banner = (fixture.nativeElement as HTMLElement).querySelector('.capture-banner.held');
    expect(banner?.textContent).toContain('the window ends');
    expect(banner?.querySelector('[aria-label="Unpause"]')).toBeTruthy();
  });

  it('recording with no window is said to be a fault, and can be stopped', () => {
    const { fixture, c } = setup(cap({ running: true }));
    fixture.detectChanges();
    expect(c.mode()).toBe('fault');
    const banner = (fixture.nativeElement as HTMLElement).querySelector('.capture-banner');
    expect(banner?.textContent).toContain('fault');
    expect(banner?.querySelector('[aria-label="Stop"]')).toBeTruthy();
  });

  it('record-for opens a window that many minutes ahead', () => {
    const { c, recordCapture } = setup();
    const before = Date.now();
    c.recordFor(60);
    const until = recordCapture.mock.calls[0][0].getTime();
    expect(until - before).toBeGreaterThanOrEqual(60 * 60_000);
    expect(until - before).toBeLessThan(60 * 60_000 + 5_000);
  });

  it('record-until takes the typed time, and refuses what is not one', () => {
    const { c, recordCapture } = setup();
    c.untilText.set('noon');
    c.recordUntil();
    expect(recordCapture).not.toHaveBeenCalled();
    c.untilText.set('18:30');
    c.recordUntil();
    const until = recordCapture.mock.calls[0][0];
    expect([until.getHours(), until.getMinutes()]).toEqual([18, 30]);
  });

  it('a press flips to the desired state as transitioning — no flap possible', () => {
    // Once, a press said one thing and the next poll the mic's stale state,
    // and the banner blinked between them.
    const { fixture, c } = setup();
    c.recordFor(30);
    expect(c.mode()).toBe('recording');
    expect(c.transitioning()).toBe(true);
    expect(c.transitionLabel()).toBe('Starting');
    fixture.detectChanges();
    const el = fixture.nativeElement as HTMLElement;
    expect(el.querySelector('.capture-banner.transitioning')?.textContent).toContain('Starting');
    // Not yet the settled banner with its buttons.
    expect(el.querySelector('.capture-banner .rec-dot')).toBeFalsy();
  });

  it('pause, unpause and stop each say what they asked for', () => {
    const { c, stopCapture, pauseCapture, unpauseCapture } = setup(cap({ until: END }));
    c.pauseCapture();
    expect(pauseCapture).toHaveBeenCalled();
    expect(c.mode()).toBe('held');
    c.unpauseCapture();
    expect(unpauseCapture).toHaveBeenCalled();
    expect(c.mode()).toBe('recording');
    c.stopCapture();
    expect(stopCapture).toHaveBeenCalled();
    expect(c.mode()).toBe('off');
  });

  it('the toggle stays enabled mid-transition, to change your mind', () => {
    const { fixture, c } = setup();
    c.recordFor(30);
    expect(c.transitioning()).toBe(true);
    fixture.detectChanges();
    const btn = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '.capture-toggle',
    );
    expect(btn?.disabled).toBe(false);
  });

  it('an unreachable mic is said out loud, not presented as fact', () => {
    const { fixture, c } = setup(cap({ settled: false, micReachable: false }));
    fixture.detectChanges();
    expect(c.unreachable()).toBe(true);
    expect(c.transitioning()).toBe(false); // unknown ≠ in-flight
    const banner = (fixture.nativeElement as HTMLElement).querySelector(
      '.capture-banner.unreachable',
    );
    expect(banner?.textContent).toContain('not reporting');
  });
});
