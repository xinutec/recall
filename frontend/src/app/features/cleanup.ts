import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  inject,
  signal,
} from '@angular/core';
import { DatePipe, DecimalPipe } from '@angular/common';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatExpansionModule } from '@angular/material/expansion';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSnackBar } from '@angular/material/snack-bar';

import { QuietScan, QuietSpan } from '../models';
import { RecallApi } from '../recall-api';
import { Waveform } from './waveform';

const MIN_SECONDS = 300;
/** The scan is ~20 minutes of ffmpeg; a couple of seconds is a live-enough progress bar. */
const POLL_MS = 2000;

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
    DecimalPipe,
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
  readonly scan = signal<QuietScan | null>(null);

  protected readonly scanning = computed(() => this.scan()?.running ?? false);
  protected readonly measured = computed(() => this.scan()?.measured ?? 0);
  protected readonly total = computed(() => this.scan()?.total ?? 0);
  protected readonly analysed = computed(() => this.scan()?.analysed ?? 0);
  protected readonly toAnalyse = computed(() => this.scan()?.toAnalyse ?? 0);
  /** The speech detector is the slow pass and the one a deletion rests on, so it is what
   * the bar tracks once the cheap sweep is done. Spans appear only as it catches up. */
  protected readonly listening = computed(() => {
    const scan = this.scan();
    return !!scan && scan.measured >= scan.total && scan.analysed < scan.toAnalyse;
  });
  protected readonly percent = computed(() => {
    const scan = this.scan();
    if (!scan) {
      return 0;
    }
    return this.listening()
      ? (scan.analysed / Math.max(scan.toAnalyse, 1)) * 100
      : (scan.measured / Math.max(scan.total, 1)) * 100;
  });
  /** True once every segment has been measured *and heard*. Until then the span list is
   * incomplete, and it says so rather than looking finished. */
  protected readonly complete = computed(() => {
    const scan = this.scan();
    return (
      !!scan &&
      !scan.running &&
      scan.measured >= scan.total &&
      scan.analysed >= scan.toAnalyse &&
      scan.total > 0
    );
  });

  /** What each open span's waveform has selected — the segments a delete would take,
   * after any trimming. Keyed by span, so trimming one never touches another. */
  private readonly selected = new Map<QuietSpan, readonly number[]>();

  private poll?: ReturnType<typeof setInterval>;

  constructor() {
    this.load();
    // A scan may already be under way — started here, from another tab, or before this
    // page was ever opened. Show it either way: the job is the server's, not the page's.
    this.refreshScan();
    inject(DestroyRef).onDestroy(() => clearInterval(this.poll));
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

  /** Ask the server to measure the archive. The work is the server's — it survives this
   * page being closed — so all this does is start it and watch. */
  protected startScan(): void {
    this.api.quietScan().subscribe((scan) => {
      this.scan.set(scan);
      this.watch();
    });
  }

  protected stopScan(): void {
    this.api.quietScanStop().subscribe((scan) => this.scan.set(scan));
  }

  private refreshScan(): void {
    this.api.quietScanProgress().subscribe((scan) => {
      this.scan.set(scan);
      if (scan.running) {
        this.watch();
      }
    });
  }

  private watch(): void {
    clearInterval(this.poll);
    this.poll = setInterval(() => {
      this.api.quietScanProgress().subscribe((scan) => {
        const finished = this.scanning() && !scan.running;
        this.scan.set(scan);
        if (finished) {
          clearInterval(this.poll);
          this.load(); // new spans have appeared behind the scan
        }
      });
    }, POLL_MS);
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
