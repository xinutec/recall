import { ChangeDetectionStrategy, Component, inject, signal } from '@angular/core';
import { DatePipe } from '@angular/common';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatExpansionModule } from '@angular/material/expansion';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSnackBar } from '@angular/material/snack-bar';

import { QuietSpan } from '../models';
import { RecallApi } from '../recall-api';
import { Waveform } from './waveform';

const MIN_SECONDS = 300;

/**
 * Prune long total-quiet capture — the mic's noise floor, no speech, most of the archive
 * and pure waste. The backend proposes the spans (one source, by raw volume); here each
 * one opens as a waveform to be looked at, played and trimmed, and only then deleted.
 * Nothing is swept automatically: destroying raw audio is irreversible, so it is always
 * a human looking at the sound they are about to lose.
 */
@Component({
  selector: 'app-cleanup',
  imports: [
    DatePipe,
    MatCardModule,
    MatButtonModule,
    MatExpansionModule,
    MatIconModule,
    MatProgressBarModule,
    Waveform,
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

  /** What each open span's waveform has selected — the segments a delete would take,
   * after any trimming. Keyed by span, so trimming one never touches another. */
  private readonly selected = new Map<QuietSpan, readonly number[]>();

  constructor() {
    this.load();
  }

  private load(): void {
    this.loading.set(true);
    this.api.quietSpans(MIN_SECONDS).subscribe({
      next: (result) => {
        this.spans.set(result.items);
        this.selected.clear();
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

  protected minutes(span: QuietSpan): number {
    return Math.round(span.durationS / 60);
  }

  protected selectionOf(span: QuietSpan): readonly number[] {
    return this.selected.get(span) ?? span.audioIds;
  }

  protected onSelected(span: QuietSpan, audioIds: readonly number[]): void {
    this.selected.set(span, audioIds);
  }

  protected trimmed(span: QuietSpan): boolean {
    return this.selectionOf(span).length < span.audioIds.length;
  }

  protected delete(span: QuietSpan): void {
    const audioIds = this.selectionOf(span);
    if (audioIds.length === 0) {
      return;
    }
    // Segments are a fixed length, so the count is the honest measure of what goes —
    // and after a trim it is the *only* honest one, since the span's own duration no
    // longer describes the selection.
    const minutes = Math.round((audioIds.length * span.durationS) / span.audioIds.length / 60);
    const trimmed = this.trimmed(span) ? ' (trimmed)' : '';
    if (
      !confirm(
        `Permanently delete ~${minutes} minutes of ${span.source} capture${trimmed} — ` +
          `${audioIds.length} segments? This cannot be undone.`,
      )
    ) {
      return;
    }
    this.api.quietDelete({ audioIds: [...audioIds] }).subscribe({
      next: (result) => {
        this.spans.update((all) => all.filter((s) => s !== span));
        this.selected.delete(span);
        const mb = (result.freedBytes / 1e6).toFixed(1);
        this.snack.open(`Deleted ${result.deleted} segments, freed ${mb} MB`, 'OK', {
          duration: 4000,
        });
      },
      error: () => this.snack.open('Delete failed', 'OK', { duration: 4000 }),
    });
  }
}
