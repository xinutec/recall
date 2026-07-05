import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { BreakpointObserver, Breakpoints } from '@angular/cdk/layout';
import { map } from 'rxjs';
import { MatToolbarModule } from '@angular/material/toolbar';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';
import { MatMenuModule } from '@angular/material/menu';
import { MatTooltipModule } from '@angular/material/tooltip';

import { BUILD_INFO } from './build-info';
import { dayKey, durationUntil, timeOfDay } from './format';
import { CaptureState } from './models';
import { RecallApi } from './recall-api';

interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: string;
  readonly exact: boolean;
}

const CAPTURE_POLL_MS = 30_000;

@Component({
  selector: 'app-root',
  imports: [
    RouterOutlet,
    RouterLink,
    RouterLinkActive,
    MatToolbarModule,
    MatButtonModule,
    MatIconModule,
    MatMenuModule,
    MatTooltipModule,
  ],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class App {
  private readonly breakpoints = inject(BreakpointObserver);
  private readonly api = inject(RecallApi);

  /** Phone-sized viewport → bottom nav; otherwise nav lives in the top toolbar. */
  protected readonly handset = toSignal(
    this.breakpoints.observe(Breakpoints.Handset).pipe(map((state) => state.matches)),
    { initialValue: false },
  );

  protected readonly nav: readonly NavItem[] = [
    { path: '/', label: 'Timeline', icon: 'forum', exact: true },
    { path: '/sessions', label: 'Sessions', icon: 'event', exact: false },
    { path: '/train', label: 'Train', icon: 'school', exact: false },
    { path: '/search', label: 'Search', icon: 'search', exact: false },
    { path: '/ask', label: 'Ask', icon: 'question_answer', exact: false },
  ];

  /** Secondary pages — in the hamburger menu, keeping the nav to five slots. */
  protected readonly more: readonly NavItem[] = [
    { path: '/compare', label: 'Compare', icon: 'difference', exact: false },
    { path: '/labels', label: 'Labels', icon: 'label', exact: false },
  ];

  // Capture (whole-house recording) state, so it can be paused while working in
  // the room. Polled so the banner reflects the worker's auto-resume.
  private readonly capture = signal<CaptureState>({ running: true, pausedUntil: null });
  // Ticks so the "resumes in Xh Ym" countdown stays current between polls.
  private readonly now = signal(Date.now());
  protected readonly paused = computed(() => this.capture().pausedUntil !== null);
  protected readonly resumeBy = computed(() => {
    const until = this.capture().pausedUntil;
    // yyyy-mm-dd before the local time, so an overnight pause reads unambiguously.
    return until ? `${dayKey(until)} ${timeOfDay(until)}` : '';
  });
  // Time left until the worker auto-resumes, e.g. "5h 23m".
  protected readonly resumeIn = computed(() => {
    const until = this.capture().pausedUntil;
    return until ? durationUntil(until, this.now()) : '';
  });

  constructor() {
    this.refreshCapture();
    setInterval(() => {
      this.refreshCapture();
      this.now.set(Date.now());
    }, CAPTURE_POLL_MS);
  }

  private refreshCapture(): void {
    this.api.capture().subscribe({ next: (s) => this.capture.set(s), error: () => undefined });
  }

  protected pauseCapture(): void {
    this.api.pauseCapture().subscribe({ next: (s) => this.capture.set(s), error: () => undefined });
  }

  protected resumeCapture(): void {
    this.api
      .resumeCapture()
      .subscribe({ next: (s) => this.capture.set(s), error: () => undefined });
  }

  /** Build stamp embedded at build time; shown in the footer so a stale cached
   * tab reveals its own old sha instead of looking current. */
  protected readonly build = BUILD_INFO;
  protected readonly builtAt = BUILD_INFO.builtAt
    ? new Date(BUILD_INFO.builtAt).toLocaleString()
    : '';
}
