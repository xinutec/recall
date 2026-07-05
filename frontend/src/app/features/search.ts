import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
} from '@angular/core';
import { httpResource } from '@angular/common/http';
import { ActivatedRoute, Router } from '@angular/router';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatIconModule } from '@angular/material/icon';

import { TranscriptList } from '../models';
import { TranscriptCard } from '../shared/transcript-card';

const SEARCH_DEBOUNCE_MS = 200;

/** Full-text search across all transcripts (SQLite FTS5 behind /api/search). */
@Component({
  selector: 'app-search',
  imports: [
    MatFormFieldModule,
    MatInputModule,
    MatProgressBarModule,
    MatIconModule,
    TranscriptCard,
  ],
  templateUrl: './search.html',
  styleUrl: './search.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Search {
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);

  /** URL is the source of truth for the query (bookmarkable, reload-safe).
   * withComponentInputBinding passes undefined when ?q is absent (it overrides
   * the default) — normalize it, or the box renders the string "undefined" and
   * .trim() throws. */
  readonly q = input('', { transform: (value: string | undefined) => value ?? '' });
  protected readonly query = signal('');

  constructor() {
    effect(() => this.query.set(this.q()));
    effect((onCleanup) => {
      const value = this.query();
      const timer = setTimeout(() => this.settledQuery.set(value), SEARCH_DEBOUNCE_MS);
      onCleanup(() => clearTimeout(timer));
    });
  }

  /** Mirror typing into the URL (replaceUrl so it doesn't spam history). */
  protected onInput(value: string): void {
    this.query.set(value);
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { q: value || null },
      replaceUrl: true,
    });
  }

  // Debounced copy of `query`: the resource keys on this, so a phone keyboard
  // doesn't fire a 200-row FTS request per keystroke — only once typing pauses.
  private readonly settledQuery = signal('');
  protected readonly trimmed = computed(() => this.settledQuery().trim());
  protected readonly trimmedLength = computed(() => this.trimmed().length);

  protected readonly results = httpResource<TranscriptList>(() => {
    const q = this.trimmed();
    if (q.length < 2) {
      return undefined;
    }
    const params = new URLSearchParams({ q, limit: '200' });
    return `/api/search?${params.toString()}`;
  });

  protected readonly items = computed(() => this.results.value()?.items ?? []);
}
