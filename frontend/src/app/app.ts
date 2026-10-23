import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { BreakpointObserver, Breakpoints } from '@angular/cdk/layout';
import { Observable, map, timeout } from 'rxjs';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatIconModule } from '@angular/material/icon';
import { MatMenuModule } from '@angular/material/menu';
import { MatSnackBar } from '@angular/material/snack-bar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Scaffold } from '@xinutec/ui-scaffold';

import { AuthState } from './auth';
import { BUILD_INFO } from './build-info';
import { dayKey, durationUntil, nextTimeOfDay, timeOfDay } from './format';
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
// only pace the edges: a pause between polls, and a retry after an error.
const CAPTURE_WAIT_S = 25;
const CAPTURE_REPOLL_MS = 250;
const CAPTURE_RETRY_MS = 5_000;

type CaptureMode = 'off' | 'recording' | 'held' | 'fault';

/** The windows one press opens, in minutes. */
const SPANS: readonly { readonly label: string; readonly minutes: number }[] = [
  { label: '30 minutes', minutes: 30 },
  { label: '1 hour', minutes: 60 },
  { label: '2 hours', minutes: 120 },
  { label: '4 hours', minutes: 240 },
];

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
  private readonly snack = inject(MatSnackBar);

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

  // Capture state, polled so the banner follows a window ending on its own.
  // `desired*` changes at the button press, `running` when the mic confirms; in
  // between the banner says "Starting…", "Stopping…" or "Pausing…".
  private readonly capture = signal<CaptureState>({
    running: false,
    until: null,
    held: false,
    desiredRunning: false,
    desiredUntil: null,
    desiredHeld: false,
    settled: true,
    micReachable: true,
    stateToken: '',
  });
  // Ticks so the "45m left" countdown stays current between polls.
  private readonly now = signal(Date.now());
  /** What was asked for: off, a window recording or paused, or recording with
   * no window, which is a recorder's fault. */
  protected readonly mode = computed<CaptureMode>(() => {
    const c = this.capture();
    if (c.desiredUntil === null) {
      return c.desiredRunning ? 'fault' : 'off';
    }
    return c.desiredHeld ? 'held' : 'recording';
  });
  protected readonly transitioning = computed(() => {
    const c = this.capture();
    return c.micReachable && !c.settled;
  });
  protected readonly transitionLabel = computed(
    () =>
      ({ off: 'Stopping', recording: 'Starting', held: 'Pausing', fault: 'Starting' })[
        this.mode()
      ],
  );
  // The mic stopped reporting, so its state is unknown.
  protected readonly unreachable = computed(() => !this.capture().micReachable);
  /** The window's end, with the date unless it is today. */
  protected readonly endsAt = computed(() => {
    const until = this.capture().desiredUntil;
    if (!until) {
      return '';
    }
    const today = dayKey(new Date(this.now()).toISOString()) === dayKey(until);
    return today ? timeOfDay(until) : `${dayKey(until)} ${timeOfDay(until)}`;
  });
  // "5h 23m".
  protected readonly endsIn = computed(() => {
    const until = this.capture().desiredUntil;
    return until ? durationUntil(until, this.now()) : '';
  });
  /** One press opens a window this long. */
  protected readonly spans = SPANS;
  /** The "until" field's text, `hh:mm`. */
  protected readonly untilText = signal('');

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
        .capture(this.capture().stateToken, CAPTURE_WAIT_S)
        // The server answers within CAPTURE_WAIT_S; slower is a lost socket.
        .pipe(timeout({ first: (CAPTURE_WAIT_S + 10) * 1_000 }))
        .subscribe({
          next: (s) => {
            this.capture.set(s);
            this.pollCapture(CAPTURE_REPOLL_MS);
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

  /** Record for `minutes` from now. */
  protected recordFor(minutes: number): void {
    this.press(this.api.recordCapture(new Date(Date.now() + minutes * 60_000)), 'start');
  }

  /** Record until the time in the field. */
  protected recordUntil(): void {
    const until = nextTimeOfDay(this.untilText(), new Date());
    if (until === null) {
      this.snack.open('Type a time like 18:30', 'Dismiss', { duration: 5000 });
      return;
    }
    this.press(this.api.recordCapture(until), 'start');
  }

  protected stopCapture(): void {
    this.press(this.api.stopCapture(), 'stop');
  }

  protected pauseCapture(): void {
    this.press(this.api.pauseCapture(), 'pause');
  }

  protected unpauseCapture(): void {
    this.press(this.api.unpauseCapture(), 'unpause');
  }

  // A press that silently failed is the one this must say: the state is not
  // what was asked for.
  private press(request: Observable<CaptureState>, verb: string): void {
    request.subscribe({
      next: (s) => this.capture.set(s),
      error: () => this.snack.open(`Could not ${verb} recording`, 'Dismiss', { duration: 5000 }),
    });
  }

  /** Shown in the footer, so a stale cached tab shows its old sha. */
  protected readonly build = BUILD_INFO;
  protected readonly builtAt = BUILD_INFO.builtAt
    ? new Date(BUILD_INFO.builtAt).toLocaleString()
    : '';
}
