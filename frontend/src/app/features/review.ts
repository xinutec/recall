import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { httpResource } from '@angular/common/http';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatChipsModule } from '@angular/material/chips';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Transcript, TranscriptList } from '../models';
import { RecallApi } from '../recall-api';
import { formatClock, formatConfidence, formatDuration } from '../format';

/**
 * Lowest-confidence transcripts first; edit the text to file a human correction
 * that supersedes the machine guess (never deletes it).
 */
@Component({
  selector: 'app-review',
  imports: [
    MatCardModule,
    MatButtonModule,
    MatFormFieldModule,
    MatInputModule,
    MatChipsModule,
    MatIconModule,
    MatProgressBarModule,
  ],
  templateUrl: './review.html',
  styleUrl: './review.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Review {
  private readonly api = inject(RecallApi);
  private readonly snack = inject(MatSnackBar);

  /** Optional comma-separated turn ids from the route query (?ids=404,405,...). */
  readonly ids = input<string>();

  /** Showing a hand-picked set of fragments rather than the low-confidence queue. */
  protected readonly picked = computed(() => (this.ids() ?? '').trim().length > 0);

  protected readonly queue = httpResource<TranscriptList>(() => {
    const ids = (this.ids() ?? '').trim();
    return ids ? `/api/transcripts?ids=${encodeURIComponent(ids)}` : '/api/review?limit=50';
  });
  protected readonly items = computed(() => this.queue.value()?.items ?? []);

  /** transcript id -> in-progress edited text */
  private readonly drafts = signal<ReadonlyMap<number, string>>(new Map());
  private readonly saving = signal<ReadonlySet<number>>(new Set());

  protected readonly clock = (t: Transcript): string => formatClock(t.start);
  protected readonly duration = formatDuration;
  protected readonly confidence = (t: Transcript): string => formatConfidence(t.confidence);

  protected draft(t: Transcript): string {
    return this.drafts().get(t.id) ?? t.text;
  }

  protected isSaving(t: Transcript): boolean {
    return this.saving().has(t.id);
  }

  protected isDirty(t: Transcript): boolean {
    const d = this.drafts().get(t.id);
    return d !== undefined && d.trim() !== t.text.trim();
  }

  protected edit(t: Transcript, text: string): void {
    const next = new Map(this.drafts());
    next.set(t.id, text);
    this.drafts.set(next);
  }

  protected unhide(t: Transcript): void {
    this.api.unhide(t.id).subscribe({
      next: () => {
        this.snack.open('Restored', undefined, { duration: 2000 });
        this.queue.reload();
      },
      error: () => this.snack.open('Could not restore', 'Dismiss', { duration: 4000 }),
    });
  }

  protected save(t: Transcript): void {
    const text = this.draft(t).trim();
    if (!text || !this.isDirty(t)) {
      return;
    }
    this.saving.update((s) => new Set(s).add(t.id));
    this.api.correct(t.id, text).subscribe({
      next: () => {
        this.snack.open('Correction saved', undefined, { duration: 2000 });
        this.clearDraft(t.id);
        this.queue.reload();
      },
      error: () => {
        this.snack.open('Could not save correction', 'Dismiss', { duration: 4000 });
        this.releaseSaving(t.id);
      },
    });
  }

  private clearDraft(id: number): void {
    const next = new Map(this.drafts());
    next.delete(id);
    this.drafts.set(next);
    this.releaseSaving(id);
  }

  private releaseSaving(id: number): void {
    const next = new Set(this.saving());
    next.delete(id);
    this.saving.set(next);
  }
}
