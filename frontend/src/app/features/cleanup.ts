import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from '@angular/core';
import { DatePipe } from '@angular/common';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSnackBar } from '@angular/material/snack-bar';

import { QuietSpan } from '../models';
import { RecallApi } from '../recall-api';

const MIN_SECONDS = 300;

/**
 * Prune long total-quiet capture — the mic noise floor, no speech, most of the
 * archive and pure waste. The backend proposes the spans (by raw volume); here you
 * play each to confirm it's quiet, then delete it (segments + Opus files, permanent).
 */
@Component({
  selector: 'app-cleanup',
  imports: [
    DatePipe,
    MatCardModule,
    MatButtonModule,
    MatIconModule,
    MatProgressBarModule,
  ],
  templateUrl: './cleanup.html',
  styleUrl: './cleanup.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Cleanup {
  private readonly api = inject(RecallApi);
  private readonly snack = inject(MatSnackBar);

  // dev-lint: allow-component-list cleanup spans must be re-derived on each visit —
  // a cached list could offer already-deleted spans; freshness is the point here.
  readonly spans = signal<readonly QuietSpan[]>([]);
  readonly loading = signal(true);
  readonly scanning = signal(false);
  readonly measured = signal(0);

  constructor() {
    this.load();
  }

  private load(): void {
    this.loading.set(true);
    this.api.quietSpans(MIN_SECONDS).subscribe({
      next: (result) => {
        this.spans.set(result.items);
        this.loading.set(false);
      },
      error: () => this.loading.set(false),
    });
  }

  /** Measure the archive one batch at a time (cached), then reload the spans. */
  scan(): void {
    this.scanning.set(true);
    const step = (): void => {
      this.api.quietScan().subscribe({
        next: (result) => {
          this.measured.update((n) => n + result.measured);
          if (result.measured > 0) {
            step();
          } else {
            this.scanning.set(false);
            this.load();
          }
        },
        error: () => this.scanning.set(false),
      });
    };
    step();
  }

  audioUrl(span: QuietSpan): string {
    return this.api.quietAudioUrl(span.audioIds[0]);
  }

  minutes(span: QuietSpan): number {
    return Math.round(span.durationS / 60);
  }

  delete(span: QuietSpan): void {
    const mins = this.minutes(span);
    if (
      !confirm(
        `Permanently delete this ${mins}-minute quiet span ` +
          `(${span.audioIds.length} segments)? This cannot be undone.`,
      )
    ) {
      return;
    }
    this.api.quietDelete({ audioIds: [...span.audioIds] }).subscribe({
      next: (result) => {
        this.spans.update((all) => all.filter((s) => s !== span));
        const mb = (result.freedBytes / 1e6).toFixed(1);
        this.snack.open(
          `Deleted ${result.deleted} segments, freed ${mb} MB`,
          'OK',
          { duration: 4000 },
        );
      },
      error: () => this.snack.open('Delete failed', 'OK', { duration: 4000 }),
    });
  }
}
