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
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { NgTemplateOutlet } from '@angular/common';
import { firstValueFrom } from 'rxjs';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';
import { MatChipsModule } from '@angular/material/chips';
import { MatMenuModule } from '@angular/material/menu';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Conversation, Moment, Transcript } from '../models';
import { RecallApi } from '../recall-api';
import { resolveSelection, SpanSel } from '../selection-span';
import { dayKey, dayLabel, formatConfidence, timeOfDay } from '../format';

/** Turn ids that continue the previous turn's *confirmed* speaker within `maxGapS` — a
 * run of consecutive same-speaker fragments whose repeated speaker header we suppress so
 * they read as one block. Only confirmed speakers coalesce; unknown turns never do (two
 * adjacent unknowns aren't necessarily the same person). A negative gap (overlap from an
 * independent-edge trim) still counts as continuing. */
export function continuationTurnIds(
  ordered: readonly {
    id: number;
    speaker: string | null;
    confirmed: boolean;
    start: string;
    end: string;
  }[],
  maxGapS = 1.0,
): Set<number> {
  const out = new Set<number>();
  for (let i = 1; i < ordered.length; i++) {
    const a = ordered[i - 1];
    const b = ordered[i];
    if (a.confirmed && b.confirmed && a.speaker && a.speaker === b.speaker) {
      const gap = (Date.parse(b.start) - Date.parse(a.end)) / 1000;
      if (gap < maxGapS) {
        out.add(b.id);
      }
    }
  }
  return out;
}

interface Day {
  readonly key: string;
  readonly label: string;
  readonly conversations: readonly Conversation[];
  /** Diarization coverage of this day's loaded machine turns. */
  readonly diarized: number;
  readonly pending: number;
}

// Turns fetched per page. The backend groups them into conversations.
const PAGE = 200;

/**
 * Chronological view of everything said, grouped into conversations (runs of
 * turns with no long silence between them) and then by day, newest at the bottom.
 *
 * One continuous, accumulating list: "Load earlier" prepends the next older page
 * and "Load later" appends the next newer one, so you scroll history either way
 * without losing your place. The URL's `?before=` records your current scroll
 * position (both buttons keep it in sync; cleared once you reach the present),
 * written with replaceUrl — a position, not a history step. On reload/share that
 * window is fetched directly — one request at the cursor — landing you back where
 * you were, and the buttons extend from there. Collapsed conversation headers are
 * cheap, so the list grows freely; turns render only when a conversation is
 * expanded.
 */
@Component({
  selector: 'app-timeline',
  imports: [
    RouterLink,
    MatButtonModule,
    MatIconModule,
    MatChipsModule,
    MatMenuModule,
    MatProgressBarModule,
    MatTooltipModule,
    NgTemplateOutlet,
  ],
  templateUrl: './timeline.html',
  styleUrl: './timeline.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Timeline {
  private readonly api = inject(RecallApi);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly snack = inject(MatSnackBar);

  /** Quick-pick roster from runtime enrolment — same as the labelling workstation.
   * Fetched, not hard-coded, so real names stay out of the codebase. */
  protected readonly speakers = signal<readonly string[]>([]);

  // Drag-select-and-assign (split a turn): the selected span, its text, the source it
  // belongs to, and an in-flight guard so a double-tap can't fire the split twice.
  protected readonly span = signal<SpanSel | null>(null);
  protected readonly spanText = signal('');
  protected readonly spanSource = signal<string | null>(null);
  protected readonly assigning = signal(false);

  // Trim one turn's audio boundary by ear. `trimVersion` cache-busts the <audio> so a
  // nudge re-fetches the re-sliced clip (the turn id is unchanged, its span isn't).
  protected readonly trimming = signal<number | null>(null);
  protected readonly trimVersion = signal(0);

  /** URL cursor: the oldest conversation start loaded so far (absent = latest only).
   * withComponentInputBinding passes undefined when the param is absent —
   * normalize so the type is honest. */
  readonly before = input('', { transform: (value: string | undefined) => value ?? '' });

  // Turns the user has tagged inline this session (id -> speaker). Overrides the
  // turn's own speaker for display until a reload brings back the corrected turn.
  private readonly relabeled = signal<ReadonlyMap<number, string>>(new Map());

  private readonly convos = signal<readonly Conversation[]>([]);
  protected readonly loading = signal(false);
  protected readonly failed = signal(false);
  protected readonly hasOlder = signal(false);
  // True only when viewing a past window (a `before` deep-link), where newer
  // history above the present isn't loaded yet. The latest window has none.
  protected readonly hasNewer = signal(false);

  // Ephemeral view state (not navigational, so not in the URL).
  private readonly expanded = signal<ReadonlySet<string>>(new Set());
  private readonly played = signal<ReadonlySet<number>>(new Set());
  // Moments whose "compare" drawer (the other mics' versions) is open this session.
  private readonly comparing = signal<ReadonlySet<string>>(new Set());
  // The cursor we last wrote to the URL ourselves (via loadEarlier), so the
  // resulting input change doesn't re-fetch what we already have.
  private lastSyncedCursor: string | null = null;

  protected readonly time = timeOfDay;
  protected readonly confidence = (t: Transcript): string => formatConfidence(t.confidence);

  /** Analysis tier of a turn, shown as its left-edge colour + a tooltip. */
  private static readonly TIER_LABELS = {
    live: 'Live — instant, rough',
    transcribed: 'Transcribed',
    diarized: 'Speaker-separated (diarized)',
    corrected: 'Human-corrected',
  } as const;
  protected tierLabel = (t: Transcript): string =>
    Timeline.TIER_LABELS[t.tier] ?? Timeline.TIER_LABELS.transcribed;

  protected readonly conversations = computed(() => this.convos());
  protected readonly empty = computed(() => this.convos().length === 0 && !this.loading());

  protected readonly days = computed<readonly Day[]>(() => {
    const groups = new Map<string, Conversation[]>();
    for (const conv of this.convos()) {
      const key = dayKey(conv.start);
      const bucket = groups.get(key);
      if (bucket) {
        bucket.push(conv);
      } else {
        groups.set(key, [conv]);
      }
    }
    return [...groups.entries()].map(([key, conversations]) => {
      let diarized = 0;
      let pending = 0;
      for (const conv of conversations) {
        // Count the spine (primary) turns only — alternates are the same moment
        // from other mics, so they'd double-count the day's coverage.
        for (const m of conv.moments) {
          for (const t of m.primary) {
            if (t.tier === 'diarized') diarized++;
            else if (t.tier === 'transcribed') pending++;
          }
        }
      }
      return { key, label: dayLabel(conversations[0].start), conversations, diarized, pending };
    });
  });

  /** Short "62% diarized" / "diarized" / "not yet diarized" tag for a day header. */
  protected coverageLabel = (day: Day): string => {
    const total = day.diarized + day.pending;
    if (total === 0) return '';
    if (day.pending === 0) return 'diarized';
    if (day.diarized === 0) return 'not yet diarized';
    return Math.round((100 * day.diarized) / total) + '% diarized';
  };
  protected coverageDone = (day: Day): boolean => day.pending === 0 && day.diarized > 0;

  constructor() {
    this.api.speakers().subscribe({
      next: (r) => this.speakers.set(r.names),
      error: () => undefined,
    });
    // Restore the view from the URL cursor on external navigation/reload. Skip
    // our own URL sync from loadEarlier (that change reflects what we just loaded).
    effect(() => {
      const cursor = this.before();
      untracked(() => {
        if (cursor === this.lastSyncedCursor) {
          return;
        }
        void this.restore(cursor);
      });
    });
  }

  /** Load the window at `cursor` (or the latest window when there's no cursor) —
   * one request, however far back the cursor is. "Load earlier" extends from here. */
  private async restore(cursor: string): Promise<void> {
    this.loading.set(true);
    this.failed.set(false);
    this.played.set(new Set());
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, cursor || undefined));
      this.convos.set(page.items);
      this.hasOlder.set(page.hasMore);
      // A past window (deep-link) has newer history above it; the latest does not.
      this.hasNewer.set(!!cursor);
      // Default-expand the newest conversation only on the latest view (no cursor).
      const newest = page.items.at(-1);
      this.expanded.set(!cursor && newest ? new Set([this.key(newest)]) : new Set());
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  /** Reload the current view (re-runs the restore for the current cursor). */
  protected refresh(): void {
    void this.restore(this.before());
  }

  /** Prepend the next older page, keeping the current list, and record how far
   * back we've scrolled in the URL (replaceUrl — a position, not a history step). */
  protected async loadEarlier(): Promise<void> {
    const earliest = this.convos()[0];
    if (!earliest || this.loading()) {
      return;
    }
    this.loading.set(true);
    this.failed.set(false);
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, earliest.start));
      this.hasOlder.set(page.hasMore);
      if (page.items.length === 0) {
        return;
      }
      this.convos.update((cur) => [...page.items, ...cur]);
      // Record the `before` we just paged from, URL-safe: toISOString gives the
      // `Z` form (no `+00:00` offset for the query string to mangle into a space).
      // Reload fetches exactly this window. The guard above stops it re-fetching.
      const cursor = new Date(earliest.start).toISOString();
      this.lastSyncedCursor = cursor;
      void this.router.navigate([], {
        relativeTo: this.route,
        queryParams: { before: cursor },
        replaceUrl: true,
      });
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  /** Append the next newer page at the bottom (forward paging from a past window).
   * Anchored on the newest `end` seen so the boundary conversation isn't re-split,
   * and records the new forward edge in the URL so a reload restores your place —
   * reaching the present clears the cursor (→ the latest view). */
  protected async loadLater(): Promise<void> {
    const newest = this.convos().at(-1);
    if (!newest || this.loading()) {
      return;
    }
    this.loading.set(true);
    this.failed.set(false);
    try {
      const page = await firstValueFrom(this.api.conversations(PAGE, undefined, newest.end));
      this.hasNewer.set(page.hasMore);
      if (page.items.length > 0) {
        this.convos.update((cur) => [...cur, ...page.items]);
      }
      // Move the URL cursor to the forward edge so reload lands here, not back at
      // the deep window. `before = <end>` makes restore() fetch the window ending at
      // that conversation; once there's nothing newer left, clear it to the latest.
      const edge = this.convos().at(-1) ?? newest;
      const cursor = page.hasMore ? new Date(edge.end).toISOString() : null;
      this.lastSyncedCursor = cursor ?? '';
      void this.router.navigate([], {
        relativeTo: this.route,
        queryParams: { before: cursor },
        replaceUrl: true,
      });
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  /** Clear the cursor back to the latest window. */
  protected jumpToLatest(): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { before: null },
      replaceUrl: true,
    });
  }

  /** A stable key for a conversation (no server id — its start is unique enough). */
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
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

  protected range(conv: Conversation): string {
    const from = timeOfDay(conv.start);
    const to = timeOfDay(conv.end);
    return from === to ? from : `${from}–${to}`;
  }

  /** Voice-match strength as a short percentage (e.g. "31%"), or '' if none. */
  protected pct(t: Transcript): string {
    return t.speakerConfidence === null ? '' : `${Math.round(t.speakerConfidence * 100)}%`;
  }

  /** The speaker to show: an inline tag from this session wins over the turn's own. */
  protected who(t: Transcript): string | null {
    return this.relabeled().get(t.id) ?? t.speaker;
  }

  /** Whether the shown speaker is human-confirmed (inline tag or a prior label). */
  protected tagged(t: Transcript): boolean {
    return this.relabeled().has(t.id) || t.speakerConfirmed;
  }

  // Turns that continue the previous confirmed same-speaker turn (<1s gap) — their
  // repeated speaker header is suppressed so a fragmented run reads as one block. Built
  // per conversation (never across the long gap between them); folds in inline relabels.
  protected readonly continuations = computed<Set<number>>(() => {
    const ids = new Set<number>();
    for (const conv of this.convos()) {
      const ordered = conv.moments
        .flatMap((m) => m.primary)
        .map((t) => ({
          id: t.id,
          speaker: this.who(t),
          confirmed: this.tagged(t),
          start: t.start,
          end: t.end,
        }));
      for (const id of continuationTurnIds(ordered)) {
        ids.add(id);
      }
    }
    return ids;
  });

  protected continues(t: Transcript): boolean {
    return this.continuations().has(t.id);
  }

  /** Queue a diarize-refine of this conversation's stretch (the idle daemon runs it with
   * the configured model). Source is the conversation's primary mic. */
  protected refineConversation(conv: Conversation): void {
    const turn = conv.moments.flatMap((m) => m.primary)[0];
    if (!turn?.source) {
      this.snack.open('No source to refine', 'Dismiss', { duration: 3000 });
      return;
    }
    this.api.refineRange(turn.source, conv.start, conv.end).subscribe({
      next: () =>
        this.snack.open('Queued for refinement', undefined, { duration: 2500 }),
      error: () =>
        this.snack.open('Could not queue refinement', 'Dismiss', { duration: 4000 }),
    });
  }

  /** Confirm/fix who said a turn: files a correction (keeping the text), which
   * sets the speaker and enrols the voiceprint — improving future auto-guesses. */
  protected relabel(t: Transcript, speaker: string): void {
    this.api.correct(t.id, t.text, { speaker, language: t.language ?? 'nl' }).subscribe({
      next: () => {
        this.relabeled.update((m) => new Map(m).set(t.id, speaker));
        this.snack.open(`Tagged ${speaker}`, undefined, { duration: 2000 });
      },
      error: () => this.snack.open('Could not tag the speaker', 'Dismiss', { duration: 4000 }),
    });
  }

  /** On mouse/touch up over the turns: open the assign-span bar for a non-empty text
   * selection inside one source, else clear it. */
  protected onSelect(): void {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || sel.rangeCount === 0) {
      this.span.set(null);
      return;
    }
    const r = resolveSelection(sel.getRangeAt(0));
    this.span.set(r?.span ?? null);
    this.spanSource.set(r?.source ?? null);
    this.spanText.set(r ? sel.toString().trim() : '');
  }

  /** Split the selected phrase off to `name` (a new name is fine) — the same span-assign
   * the session view uses, here on a continuous-capture turn. */
  protected assignSelectedSpan(name: string): void {
    const span = this.span();
    const source = this.spanSource();
    const who = name.trim();
    if (!span || !source || !who || this.assigning()) {
      return;
    }
    this.assigning.set(true);
    this.api.assignSpan(source, { ...span, name: who }).subscribe({
      next: () => {
        this.clearSpan();
        this.refresh();
        this.assigning.set(false);
      },
      error: () => {
        this.assigning.set(false);
        this.snack.open('Could not split the turn', 'Dismiss', { duration: 4000 });
      },
    });
  }

  protected clearSpan(): void {
    window.getSelection()?.removeAllRanges();
    this.span.set(null);
  }

  protected isPlayed(t: Transcript): boolean {
    return this.played().has(t.id);
  }

  protected play(t: Transcript): void {
    this.played.update((s) => new Set(s).add(t.id));
  }

  protected isTrimming(t: Transcript): boolean {
    return this.trimming() === t.id;
  }

  /** Audio URL for a turn — cache-busted while trimming so each nudge re-fetches the
   * re-sliced clip (the turn id is unchanged, but its span isn't). */
  protected audioSrc(t: Transcript): string {
    return this.isTrimming(t) ? `${t.audioUrl}?v=${this.trimVersion()}` : t.audioUrl;
  }

  protected startTrim(t: Transcript): void {
    this.trimming.set(t.id);
    this.play(t); // render the <audio> so the current cut is audible
  }

  protected stopTrim(): void {
    this.trimming.set(null);
  }

  /** Move one edge of a turn ±delta by ear, then replay the re-sliced clip. Hand-tune a
   * boundary the (char-estimated, word-timing-less) split got wrong. */
  protected nudgeTurn(t: Transcript, edge: 'start' | 'end', delta: number): void {
    this.api.nudgeTurn(t.id, edge, delta).subscribe({
      next: () => {
        this.play(t);
        this.trimVersion.update((v) => v + 1);
      },
      error: () =>
        this.snack.open('Could not move the boundary', 'Dismiss', { duration: 4000 }),
    });
  }

  /** How many mics caught this moment — drives the "N mics" corroboration badge. */
  protected mics(m: Moment): number {
    return m.sources.length;
  }

  protected isComparing(m: Moment): boolean {
    return this.comparing().has(m.start);
  }

  /** Open/close a moment's compare drawer (the other mics' versions of it). */
  protected toggleCompare(m: Moment): void {
    this.comparing.update((set) => {
      const next = new Set(set);
      if (next.has(m.start)) {
        next.delete(m.start);
      } else {
        next.add(m.start);
      }
      return next;
    });
  }
}
