import {
  ChangeDetectionStrategy,
  Component,
  OnDestroy,
  effect,
  inject,
  signal,
} from '@angular/core';
import { httpResource } from '@angular/common/http';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatProgressBarModule } from '@angular/material/progress-bar';

import { AskAnswer, DaySummaryList, TodaySummary } from '../models';
import { RecallApi } from '../recall-api';
import { timeOfDay } from '../format';
import { TranscriptCard } from '../shared/transcript-card';

// Re-poll cadence while the backend regenerates the today-summary (generation
// takes ~6–90s; the endpoint serves stale text meanwhile, so this only decides
// how quickly the fresh text replaces it).
const TODAY_POLL_MS = 7000;

/** The recall layer: ask the archive a question (grounded, cited), a live
 * "today so far" summary, and the settled per-day summaries the refine daemon
 * generates. */
@Component({
  selector: 'app-ask',
  imports: [
    MatButtonModule,
    MatCardModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatProgressBarModule,
    TranscriptCard,
  ],
  templateUrl: './ask.html',
  styleUrl: './ask.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Ask implements OnDestroy {
  private readonly api = inject(RecallApi);

  protected readonly question = signal('');
  protected readonly asking = signal(false);
  protected readonly failed = signal(false);
  // null = nothing asked yet; an AskAnswer with answer=null = honest "not found".
  protected readonly result = signal<AskAnswer | null>(null);

  protected readonly summaries = httpResource<DaySummaryList>(() => '/api/summaries');
  protected readonly today = httpResource<TodaySummary>(() => '/api/summaries/today');
  protected readonly asOf = timeOfDay;
  private pollTimer: ReturnType<typeof setTimeout> | null = null;

  constructor() {
    // Stale-while-revalidate, client half: while the backend says a
    // regeneration is pending, re-poll until the fresh text lands; then stop —
    // no timer runs when the summary is up to date.
    effect(() => {
      const t = this.today.value();
      this.clearPoll();
      if (t?.pending) {
        this.pollTimer = setTimeout(() => this.today.reload(), TODAY_POLL_MS);
      }
    });
  }

  private clearPoll(): void {
    if (this.pollTimer !== null) {
      clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  ngOnDestroy(): void {
    this.clearPoll();
  }

  protected submit(): void {
    const q = this.question().trim();
    if (!q || this.asking()) {
      return;
    }
    this.asking.set(true);
    this.failed.set(false);
    this.result.set(null);
    // Generation is local and slow (first call also loads the model) — the
    // template shows progress until this lands.
    this.api.ask(q).subscribe({
      next: (r) => {
        this.result.set(r);
        this.asking.set(false);
      },
      error: () => {
        this.failed.set(true);
        this.asking.set(false);
      },
    });
  }
}
