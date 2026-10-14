import {
  afterNextRender,
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  Injector,
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
/** How much earlier than a line, opened from elsewhere, the window starts. */
const LEAD_MS = 60 * 60 * 1000;

/**
 * Everything said, as conversations grouped by day, newest at the bottom.
 *
 * Load earlier / Load later extend one list. `?before=` records the position
 * (replaceUrl: a position, not a history step), so a reload fetches that window
 * in one request. `?at=&line=`, from a search result, opens the window around
 * that line with its conversation open and the line marked.
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
  private readonly injector = inject(Injector);

  /** Fetched, so real names stay out of the code. */
  private readonly roster = inject(Speakers);
  protected readonly speakers = this.roster.names;

  readonly before = input('', { transform: (value: string | undefined) => value ?? '' });
  /** An instant to open at, with `line`, the id of the line said then. */
  readonly at = input('', { transform: (value: string | undefined) => value ?? '' });
  readonly line = input('', { transform: (value: string | undefined) => value ?? '' });
  /** The line opened at, marked in its conversation. */
  protected readonly focus = computed(() => Number(this.line()) || null);

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
      for (const t of conversations.flatMap((c) => c.moments).map((m) => m.primary)) {
        if (t.tier === 'diarized') diarized++;
        else if (t.tier === 'transcribed') pending++;
      }
      return { key, label: dayLabel(conversations[0].start), conversations, diarized, pending };
    });
  });

  /** Said only while lines wait for speaker separation; a finished day says nothing. */
  protected coverageLabel = (day: Day): string => {
    if (day.pending === 0) return '';
    if (day.diarized === 0) return 'speakers not yet separated';
    const share = Math.round((100 * day.diarized) / (day.diarized + day.pending));
    return `speakers separated in ${share}%`;
  };

  constructor() {
    this.roster.refresh();
    effect(() => {
      const cursor = this.before();
      const at = this.at();
      untracked(() => {
        if (at) void this.openAt(at);
        else if (cursor !== this.lastSyncedCursor) void this.restore(cursor);
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

  /** The window from an hour before `at`, with the conversation holding it open. */
  private async openAt(at: string): Promise<void> {
    const from = new Date(new Date(at).getTime() - LEAD_MS).toISOString();
    this.loading.set(true);
    this.failed.set(false);
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, undefined, from));
      this.convos.set(page.items);
      this.hasOlder.set(true);
      this.hasNewer.set(page.hasMore);
      const its = page.items.find((c) => c.start <= at && at <= c.end);
      this.expanded.set(new Set(its ? [this.key(its)] : []));
      afterNextRender(() => this.reveal(), { injector: this.injector });
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  /** Scroll the marked line into view. */
  private reveal(): void {
    document.querySelector(`[data-id="${this.focus()}"]`)?.scrollIntoView({ block: 'center' });
  }

  protected refresh(): void {
    if (this.at()) void this.openAt(this.at());
    else void this.restore(this.before(), true);
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
      queryParams: { before: cursor, at: null, line: null },
      replaceUrl: true,
    });
  }

  protected jumpToLatest(): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { before: null, at: null, line: null },
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
