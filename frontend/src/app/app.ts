import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { BreakpointObserver, Breakpoints } from '@angular/cdk/layout';
import { map, timeout } from 'rxjs';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatIconModule } from '@angular/material/icon';
import { MatMenuModule } from '@angular/material/menu';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Scaffold } from '@xinutec/ui-scaffold';

import { AuthState } from './auth';
import { BUILD_INFO } from './build-info';
import { dayKey, durationUntil, timeOfDay } from './format';
import { CaptureState } from './models';
import { RecallApi } from './recall-api';
import { Telemetry } from './telemetry';
import { SwUpdates } from './sw-updates';

interface NavItem {
  readonly path: string;
  readonly label: string;
  readonly icon: string;
  readonly exact: boolean;
}

// The server holds /api/capture?wait&known until the state changes, so these
// only pace the edges: a pause between polls, a retry after an error, and a
// plain poll when the answer has no stateToken.
const CAPTURE_WAIT_S = 25;
const CAPTURE_REPOLL_MS = 250;
const CAPTURE_RETRY_MS = 5_000;
const CAPTURE_PLAIN_POLL_MS = 5_000;

@Component({
  selector: 'app-root',
  imports: [
    RouterOutlet,
    RouterLink,
    RouterLinkActive,
    MatButtonModule,
    MatCardModule,
    MatIconModule,
    MatMenuModule,
    MatTooltipModule,
    Scaffold,
  ],
  templateUrl: './app.html',
  styleUrl: './app.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class App {
  private readonly breakpoints = inject(BreakpointObserver);
  private readonly api = inject(RecallApi);

  /** The Nextcloud sign-in wall. */
  protected readonly auth = inject(AuthState);
  private readonly telemetry = inject(Telemetry);
  private readonly swUpdates = inject(SwUpdates);

  /** Phone-sized: a bottom nav; otherwise a rail beside the content. */
  protected readonly handset = toSignal(
    this.breakpoints.observe(Breakpoints.Handset).pipe(map((state) => state.matches)),
    { initialValue: false },
  );

  protected readonly nav: readonly NavItem[] = [
    { path: '/', label: 'Timeline', icon: 'forum', exact: true },
    { path: '/sessions', label: 'Sessions', icon: 'event', exact: false },
    { path: '/check', label: 'Check', icon: 'spellcheck', exact: false },
    { path: '/search', label: 'Search', icon: 'search', exact: false },
  ];

  /** Screens drilled into from the menu, each declaring its way up. */
  protected readonly more: readonly NavItem[] = [
    { path: '/labels', label: 'Labels', icon: 'label', exact: false },
  ];

  // Capture state, polled so the banner follows an automatic resume. `desired*`
  // changes at the button press, `running` when the mic confirms; in between
  // the banner says "Pausing…" or "Resuming…".
  private readonly capture = signal<CaptureState>({
    running: true,
    pausedUntil: null,
    desiredRunning: true,
    desiredPausedUntil: null,
    settled: true,
    micReachable: true,
    stateToken: '',
  });
  // Ticks so the "resumes in Xh Ym" countdown stays current between polls.
  private readonly now = signal(Date.now());
  protected readonly paused = computed(() => this.capture().desiredPausedUntil !== null);
  protected readonly transitioning = computed(() => {
    const c = this.capture();
    return c.micReachable && !c.settled;
  });
  protected readonly transitionLabel = computed(() =>
    this.capture().desiredRunning ? 'Resuming' : 'Pausing',
  );
  // The mic stopped reporting, so its state is unknown.
  protected readonly unreachable = computed(() => !this.capture().micReachable);
  protected readonly resumeBy = computed(() => {
    const until = this.capture().desiredPausedUntil;
    // With the date, so an overnight pause is unambiguous.
    return until ? `${dayKey(until)} ${timeOfDay(until)}` : '';
  });
  // "5h 23m".
  protected readonly resumeIn = computed(() => {
    const until = this.capture().desiredPausedUntil;
    return until ? durationUntil(until, this.now()) : '';
  });

  constructor() {
    this.telemetry.init();
    this.swUpdates.start();
    this.pollCapture(0);
    setInterval(() => this.now.set(Date.now()), 30_000);
  }

  // One poll at a time: each answer or error schedules the next.
  private pollTimer: ReturnType<typeof setTimeout> | null = null;

  private pollCapture(delayMs: number): void {
    if (this.pollTimer !== null) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
    const go = () => {
      this.api
        .capture(this.capture().stateToken ?? '', CAPTURE_WAIT_S)
        // The server answers within CAPTURE_WAIT_S; slower is a lost socket.
        .pipe(timeout({ first: (CAPTURE_WAIT_S + 10) * 1_000 }))
        .subscribe({
          next: (s) => {
            this.capture.set(s);
            this.pollCapture(s.stateToken ? CAPTURE_REPOLL_MS : CAPTURE_PLAIN_POLL_MS);
          },
          error: () => this.pollCapture(CAPTURE_RETRY_MS),
        });
    };
    // Synchronously at 0, so the first poll starts during construction.
    if (delayMs <= 0) {
      go();
    } else {
      this.pollTimer = setTimeout(go, delayMs);
    }
  }

  protected pauseCapture(): void {
    this.api
      .pauseCapture()
      .subscribe({ next: (s) => this.capture.set(s), error: () => undefined });
  }

  protected resumeCapture(): void {
    this.api
      .resumeCapture()
      .subscribe({ next: (s) => this.capture.set(s), error: () => undefined });
  }

  /** Shown in the footer, so a stale cached tab shows its old sha. */
  protected readonly build = BUILD_INFO;
  protected readonly builtAt = BUILD_INFO.builtAt
    ? new Date(BUILD_INFO.builtAt).toLocaleString()
    : '';
}
