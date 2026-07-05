import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { httpResource } from '@angular/common/http';
import { ActivatedRoute, Router } from '@angular/router';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatChipsModule } from '@angular/material/chips';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { MatSnackBar } from '@angular/material/snack-bar';

import { HouseholdContext, Label, LabelList, VocabularyList } from '../models';
import { RecallApi } from '../recall-api';
import { formatClock } from '../format';

/**
 * Review/audit the labelled fragments: filter by voice, play each, re-tag a
 * mis-assigned speaker, or remove a bad label from the corpus.
 */
@Component({
  selector: 'app-labels',
  imports: [
    MatCardModule,
    MatButtonModule,
    MatChipsModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatProgressBarModule,
    MatTooltipModule,
  ],
  templateUrl: './labels.html',
  styleUrl: './labels.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Labels {
  private readonly api = inject(RecallApi);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly snack = inject(MatSnackBar);

  // The household vocabulary: proper nouns the ASR is biased toward. Managed
  // here because this is the curation page — a new term applies from the next
  // transcription, no restart.
  protected readonly vocabulary = httpResource<VocabularyList>(() => '/api/vocabulary');
  protected readonly vocabTerms = computed(() => this.vocabulary.value()?.items ?? []);
  protected readonly newTerm = signal('');

  protected addTerm(): void {
    const term = this.newTerm().trim();
    if (!term) {
      return;
    }
    this.api.addVocabularyTerm(term).subscribe({
      next: () => {
        this.newTerm.set('');
        this.vocabulary.reload();
      },
      error: () => this.snack.open('Could not add the term', 'Dismiss', { duration: 4000 }),
    });
  }

  protected removeTerm(id: number): void {
    this.api.deleteVocabularyTerm(id).subscribe({
      next: () => this.vocabulary.reload(),
      error: () => this.snack.open('Could not remove the term', 'Dismiss', { duration: 4000 }),
    });
  }

  // Household context: background facts handed to the LLM with every summary /
  // ask prompt (pronouns, who lives here, recurring places). Data, not code —
  // curated here alongside the vocabulary. Editable copy seeded from the server
  // value once loaded; Save is enabled only when it differs.
  private readonly storedContext = httpResource<HouseholdContext>(() => '/api/context');
  protected readonly contextDraft = signal<string | null>(null);
  protected readonly contextText = computed(
    () => this.contextDraft() ?? this.storedContext.value()?.text ?? '',
  );
  protected readonly contextDirty = computed(
    () => this.contextDraft() !== null && this.contextDraft() !== this.storedContext.value()?.text,
  );

  protected saveContext(): void {
    const text = this.contextText().trim();
    this.api.setContext(text).subscribe({
      next: () => {
        this.contextDraft.set(null);
        this.storedContext.reload();
        this.snack.open('Context saved — applies from the next summary/answer', undefined, {
          duration: 3000,
        });
      },
      error: () => this.snack.open('Could not save the context', 'Dismiss', { duration: 4000 }),
    });
  }

  /** URL drives the filter (bookmarkable, e.g. linked from the Train balance).
   * withComponentInputBinding passes undefined when the param is absent —
   * normalize so the type is honest. */
  readonly speaker = input('', { transform: (value: string | undefined) => value ?? '' });
  /** Quick-pick roster from runtime enrolment (not hard-coded — keeps real names
   * out of the codebase, per the design's privacy promise). */
  private readonly roster = httpResource<{ names: string[] }>(() => '/api/speakers');
  protected readonly speakers = computed(() => this.roster.value()?.names ?? []);
  protected readonly clock = (start: string): string => formatClock(start);

  protected readonly results = httpResource<LabelList>(() => {
    const speaker = this.speaker();
    const params = new URLSearchParams();
    if (speaker) {
      params.set('speaker', speaker);
    }
    return `/api/corrections?${params.toString()}`;
  });
  protected readonly items = computed(() => this.results.value()?.items ?? []);
  private readonly counts = computed(() => this.results.value()?.bySpeaker ?? {});
  protected readonly speakerCount = (name: string): number => this.counts()[name] ?? 0;

  // Default: play the exact trimmed cut, to audit the boundaries. With context
  // adds the lead-in/-out back for easier voice recognition.
  protected readonly context = signal(false);
  protected setContext(on: boolean): void {
    this.context.set(on);
  }

  // Bumped after a nudge so the <audio> re-fetches the (now different) span.
  private readonly audioVersion = signal(0);
  protected audioSrc(label: Label): string {
    const parts: string[] = [];
    if (this.context()) {
      parts.push('context=true');
    }
    if (this.audioVersion() > 0) {
      parts.push(`v=${this.audioVersion()}`);
    }
    return parts.length ? `${label.audioUrl}?${parts.join('&')}` : label.audioUrl;
  }

  /** Move a boundary by `delta` seconds (negative = earlier, positive = later).
   * Widen a too-tight cut: start −, end +. Tighten: start +, end −. */
  protected nudge(id: number, edge: 'start' | 'end', delta: number): void {
    this.api.nudgeCorrection(id, edge, delta).subscribe({
      next: () => this.audioVersion.update((v) => v + 1),
      error: () => this.snack.open('Could not adjust the clip', 'Dismiss', { duration: 4000 }),
    });
  }

  protected pick(speaker: string): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { speaker: speaker || null },
      replaceUrl: true,
    });
  }

  protected reassign(id: number, speaker: string): void {
    this.api.reassignCorrection(id, speaker).subscribe({
      next: () => {
        this.snack.open('Re-tagged', undefined, { duration: 2000 });
        this.results.reload();
      },
      error: () => this.snack.open('Could not re-tag', 'Dismiss', { duration: 4000 }),
    });
  }

  protected remove(id: number): void {
    this.api.hideCorrection(id).subscribe({
      next: () => {
        this.snack.open('Removed from the corpus', undefined, { duration: 2000 });
        this.results.reload();
      },
      error: () => this.snack.open('Could not remove', 'Dismiss', { duration: 4000 }),
    });
  }
}
