import { ChangeDetectionStrategy, Component, computed, inject, input, signal } from '@angular/core';
import { httpResource } from '@angular/common/http';
import { ActivatedRoute, Router } from '@angular/router';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatButtonToggleModule } from '@angular/material/button-toggle';
import { MatChipsModule } from '@angular/material/chips';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { MatSnackBar } from '@angular/material/snack-bar';
import { scaffoldTitle } from '@xinutec/ui-scaffold';

import { Label, LabelList, SpeakerNames, VocabularyList } from '../models';
import { RecallApi } from '../recall-api';
import { formatClock } from '../format';
import { PlayButton } from '../shared/play-button';

/**
 * Review the labelled fragments: filter by voice, play each, reassign a wrong
 * speaker or hide a bad label. Also the household vocabulary.
 */
@Component({
  selector: 'app-labels',
  imports: [
    PlayButton,
    MatCardModule,
    MatButtonModule,
    MatButtonToggleModule,
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

  constructor() {
    scaffoldTitle(() => 'Review labels');
  }

  // Proper nouns transcription is biased toward, from the next clip on.
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

  /** The filter, from the URL. Absent arrives as undefined. */
  readonly speaker = input('', { transform: (value: string | undefined) => value ?? '' });
  /** Fetched, so real names stay out of the code. */
  private readonly roster = httpResource<SpeakerNames>(() => '/api/speakers');
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

  // Off: the exact cut, to check its edges. On: with the audio around it, to
  // recognise the voice.
  protected readonly context = signal(false);
  protected setContext(on: boolean): void {
    this.context.set(on);
  }

  protected audioSrc(label: Label): string {
    return this.context() ? `${label.audioUrl}?context=true` : label.audioUrl;
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
