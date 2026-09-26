import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  signal,
  untracked,
} from '@angular/core';
import { ActivatedRoute, Router } from '@angular/router';
import { firstValueFrom } from 'rxjs';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';
import { MatSlideToggleModule } from '@angular/material/slide-toggle';
import { MatChipsModule } from '@angular/material/chips';
import { MatProgressBarModule } from '@angular/material/progress-bar';

import { Conversation } from '../models';
import { RecallApi } from '../recall-api';
import { Player } from '../shared/player';
import { Speakers } from '../shared/speakers';
import { Turns } from '../shared/turns';
import { dayKey, dayLabel, timeOfDay } from '../format';

interface Day {
  readonly key: string;
  readonly label: string;
  readonly conversations: readonly Conversation[];
  /** Diarization coverage of the day's loaded turns. */
  readonly diarized: number;
  readonly pending: number;
}

const PAGE = 200;

/**
 * Everything said, as conversations grouped by day, newest at the bottom.
 *
 * Load earlier / Load later extend one list. `?before=` records the position
 * (replaceUrl: a position, not a history step), so a reload fetches that window
 * in one request.
 */
@Component({
  selector: 'app-timeline',
  imports: [
    MatButtonModule,
    MatIconModule,
    MatSlideToggleModule,
    MatChipsModule,
    MatProgressBarModule,
    Turns,
  ],
  templateUrl: './timeline.html',
  styleUrl: './timeline.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Timeline {
  private readonly api = inject(RecallApi);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  protected readonly player = inject(Player);

  /** Fetched, so real names stay out of the code. */
  private readonly roster = inject(Speakers);
  protected readonly speakers = this.roster.names;

  readonly before = input('', { transform: (value: string | undefined) => value ?? '' });

  private readonly convos = signal<readonly Conversation[]>([]);
  protected readonly loading = signal(false);
  protected readonly failed = signal(false);
  protected readonly hasOlder = signal(false);
  /** Only a past window (a `before` link) has newer history to load. */
  protected readonly hasNewer = signal(false);

  private readonly expanded = signal<ReadonlySet<string>>(new Set());
  // The cursor we wrote ourselves, so its input change doesn't refetch.
  private lastSyncedCursor: string | null = null;

  protected readonly time = timeOfDay;
  protected readonly empty = computed(() => this.convos().length === 0 && !this.loading());

  protected readonly days = computed<readonly Day[]>(() => {
    const groups = new Map<string, Conversation[]>();
    for (const conv of this.convos()) {
      const key = dayKey(conv.start);
      groups.set(key, [...(groups.get(key) ?? []), conv]);
    }
    return [...groups.entries()].map(([key, conversations]) => {
      let diarized = 0;
      let pending = 0;
      // Primary turns only: alternates are the same moment on other mics.
      for (const t of conversations.flatMap((c) => c.moments).flatMap((m) => m.primary)) {
        if (t.tier === 'diarized') diarized++;
        else if (t.tier === 'transcribed') pending++;
      }
      return { key, label: dayLabel(conversations[0].start), conversations, diarized, pending };
    });
  });

  protected coverageLabel = (day: Day): string => {
    const total = day.diarized + day.pending;
    if (total === 0) return '';
    if (day.pending === 0) return 'diarized';
    if (day.diarized === 0) return 'not yet diarized';
    return Math.round((100 * day.diarized) / total) + '% diarized';
  };
  protected coverageDone = (day: Day): boolean => day.pending === 0 && day.diarized > 0;

  constructor() {
    this.roster.refresh();
    effect(() => {
      const cursor = this.before();
      untracked(() => {
        if (cursor !== this.lastSyncedCursor) void this.restore(cursor);
      });
    });
  }

  /** Fetch the window at `cursor`, or the latest. `keep` holds open conversations open. */
  private async restore(cursor: string, keep = false): Promise<void> {
    this.loading.set(true);
    this.failed.set(false);
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, cursor || undefined));
      this.convos.set(page.items);
      this.hasOlder.set(page.hasMore);
      this.hasNewer.set(!!cursor);
      const newest = page.items.at(-1);
      if (!keep) {
        this.expanded.set(!cursor && newest ? new Set([this.key(newest)]) : new Set());
      }
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  protected refresh(): void {
    void this.restore(this.before(), true);
  }

  protected async loadEarlier(): Promise<void> {
    const earliest = this.convos()[0];
    if (!earliest || this.loading()) return;
    this.loading.set(true);
    this.failed.set(false);
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, earliest.start));
      this.hasOlder.set(page.hasMore);
      if (page.items.length === 0) return;
      this.convos.update((cur) => [...page.items, ...cur]);
      // The `Z` form: a `+` in the query string would come back as a space.
      this.sync(new Date(earliest.start).toISOString());
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  /** Append the next newer page, anchored on the newest end so the boundary
   * conversation isn't split twice. Reaching the present clears the cursor. */
  protected async loadLater(): Promise<void> {
    const newest = this.convos().at(-1);
    if (!newest || this.loading()) return;
    this.loading.set(true);
    this.failed.set(false);
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, undefined, newest.end));
      this.hasNewer.set(page.hasMore);
      if (page.items.length > 0) this.convos.update((cur) => [...cur, ...page.items]);
      const edge = this.convos().at(-1) ?? newest;
      this.sync(page.hasMore ? new Date(edge.end).toISOString() : null);
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  private sync(cursor: string | null): void {
    this.lastSyncedCursor = cursor ?? '';
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { before: cursor },
      replaceUrl: true,
    });
  }

  protected jumpToLatest(): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { before: null },
      replaceUrl: true,
    });
  }

  protected key(conv: Conversation): string {
    return conv.start;
  }

  protected isExpanded(conv: Conversation): boolean {
    return this.expanded().has(this.key(conv));
  }

  protected toggle(conv: Conversation): void {
    const key = this.key(conv);
    this.expanded.update((set) => {
      const next = new Set(set);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  }

  protected range(conv: Conversation): string {
    const from = timeOfDay(conv.start);
    const to = timeOfDay(conv.end);
    return from === to ? from : `${from}–${to}`;
  }
}
