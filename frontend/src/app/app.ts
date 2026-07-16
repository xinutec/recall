import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { BreakpointObserver, Breakpoints } from '@angular/cdk/layout';
import { map, timeout } from 'rxjs';
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

// Long-poll pacing: the server holds /api/capture?wait&known until the state
// actually changes (a press on any client, a mirror confirmation), so changes
// land in ~RTT. The delays below only guard the edges: a breather between
// polls, a retry gap after an error, and the plain-poll cadence against an
// older server that answers immediately (no stateToken).
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
    { path: '/cleanup', label: 'Cleanup', icon: 'delete_sweep', exact: false },
  ];

  // Capture (whole-house recording) state, so it can be paused while working in
  // the room. Polled so the banner reflects the worker's auto-resume. Rendered
  // spec-vs-status: `desired*` moves at the button press, `running` is the mic's
  // confirmed word, and the gap between them shows as "Pausing…"/"Resuming…" —
  // never a flap between the two truths.
  private readonly capture = signal<CaptureState>({
    running: true,
    pausedUntil: null,
    desiredRunning: true,
    desiredPausedUntil: null,
    settled: true,
    micReachable: true,
  });
  // Ticks so the "resumes in Xh Ym" countdown stays current between polls.
  private readonly now = signal(Date.now());
  // The banner/button follow the DESIRED state (what was asked for)…
  protected readonly paused = computed(() => this.capture().desiredPausedUntil !== null);
  // …with an explicit in-between while the mic hasn't confirmed it yet.
  protected readonly transitioning = computed(() => {
    const c = this.capture();
    return c.micReachable && !c.settled;
  });
  protected readonly transitionLabel = computed(() =>
    this.capture().desiredRunning ? 'Resuming' : 'Pausing',
  );
  // The mic stopped reporting: its true state is unknown, and saying so beats
  // presenting the intent as fact.
  protected readonly unreachable = computed(() => !this.capture().micReachable);
  protected readonly resumeBy = computed(() => {
    const until = this.capture().desiredPausedUntil;
    // yyyy-mm-dd before the local time, so an overnight pause reads unambiguously.
    return until ? `${dayKey(until)} ${timeOfDay(until)}` : '';
  });
  // Time left until the worker auto-resumes, e.g. "5h 23m".
  protected readonly resumeIn = computed(() => {
    const until = this.capture().desiredPausedUntil;
    return until ? durationUntil(until, this.now()) : '';
  });

  constructor() {
    this.pollCapture(0);
    setInterval(() => this.now.set(Date.now()), 30_000);
  }

  // One chained capture poll at a time: each response (or error) schedules the
  // next request. Against a long-polling server the request itself hangs until
  // something changes, so the chain is mostly one idle held request.
  private pollTimer: ReturnType<typeof setTimeout> | null = null;

  private pollCapture(delayMs: number): void {
    if (this.pollTimer !== null) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
    const go = () => {
      this.api
        .capture(this.capture().stateToken ?? '', CAPTURE_WAIT_S)
        // A dead connection would otherwise hold the chain forever: the server
        // answers within CAPTURE_WAIT_S, so anything slower is a lost socket.
        .pipe(timeout({ first: (CAPTURE_WAIT_S + 10) * 1_000 }))
        .subscribe({
          next: (s) => {
            this.capture.set(s);
            this.pollCapture(s.stateToken ? CAPTURE_REPOLL_MS : CAPTURE_PLAIN_POLL_MS);
          },
          error: () => this.pollCapture(CAPTURE_RETRY_MS),
        });
    };
    // delay 0 runs synchronously so the initial state is applied on construction.
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

  /** Build stamp embedded at build time; shown in the footer so a stale cached
   * tab reveals its own old sha instead of looking current. */
  protected readonly build = BUILD_INFO;
  protected readonly builtAt = BUILD_INFO.builtAt
    ? new Date(BUILD_INFO.builtAt).toLocaleString()
    : '';
}
