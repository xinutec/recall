import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { provideZonelessChangeDetection } from '@angular/core';
import { BehaviorSubject, of } from 'rxjs';
import { vi } from 'vitest';

import { App } from './app';
import { BUILD_INFO } from './build-info';
import { RecallApi } from './recall-api';
import { CaptureState } from './models';

function setup(initial: CaptureState = { running: true, pausedUntil: null }) {
  const state = new BehaviorSubject<CaptureState>(initial);
  const capture = vi.fn(() => state);
  const pauseCapture = vi.fn(() => of({ running: false, pausedUntil: '2026-06-17T20:00:00Z' }));
  const resumeCapture = vi.fn(() => of({ running: true, pausedUntil: null }));
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
    const { fixture, c } = setup({ running: false, pausedUntil: '2026-06-17T20:00:00Z' });
    fixture.detectChanges();
    expect(c.paused()).toBe(true);
    expect((fixture.nativeElement as HTMLElement).querySelector('.paused-banner')).toBeTruthy();
  });

  it('resume-by leads with a yyyy-mm-dd date before the time', () => {
    const { c } = setup({ running: false, pausedUntil: '2026-06-17T20:00:00Z' });
    // Shape, not exact value — the local date/time depend on the runner's timezone.
    expect(c.resumeBy()).toMatch(/^\d{4}-\d{2}-\d{2} \S/);
  });

  it('resume-in shows the remaining hours/minutes', () => {
    // Timezone-independent (a duration, not a wall clock): a far-future deadline.
    const { c } = setup({
      running: false,
      pausedUntil: new Date(Date.now() + 5 * 3_600_000 + 23 * 60_000).toISOString(),
    });
    expect(c.resumeIn()).toBe('5h 23m');
  });

  it('resume-in is empty when capture is running', () => {
    expect(setup().c.resumeIn()).toBe('');
  });

  it('pausing calls the API and flips to paused; resuming flips back', () => {
    const { c, pauseCapture, resumeCapture } = setup();
    expect(c.paused()).toBe(false);
    c.pauseCapture();
    expect(pauseCapture).toHaveBeenCalled();
    expect(c.paused()).toBe(true);
    c.resumeCapture();
    expect(resumeCapture).toHaveBeenCalled();
    expect(c.paused()).toBe(false);
  });
});
