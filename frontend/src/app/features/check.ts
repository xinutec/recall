import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  input,
  linkedSignal,
  signal,
  untracked,
} from '@angular/core';
import { HttpClient } from '@angular/common/http';
import { ActivatedRoute, Router } from '@angular/router';
import { firstValueFrom } from 'rxjs';
import { TextFieldModule } from '@angular/cdk/text-field';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSelectModule } from '@angular/material/select';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Moment, Transcript, TranscriptList } from '../models';
import { RecallApi } from '../recall-api';
import { dayKey, timeOfDaySeconds } from '../format';
import { Player } from '../shared/player';

/** One line per moment, never live and never one already corrected. Where several
 * mics heard the moment, the one checked least so far: a check scores the mic
 * whose words were edited, so both of two competing mics need checking. */
export function pickLines(moments: readonly Moment[]): Transcript[] {
  const counts = new Map<string, number>();
  const out: Transcript[] = [];
  for (const m of moments) {
    const all = [...m.primary, ...m.alternates];
    if (all.some((t) => t.tier === 'corrected')) continue;
    const candidates = all.filter((t) => t.tier !== 'live' && t.text.trim());
    let best: Transcript | undefined;
    for (const t of candidates) {
      const n = counts.get(t.source ?? '') ?? 0;
      if (!best || n < (counts.get(best.source ?? '') ?? 0)) best = t;
    }
    if (!best) continue;
    counts.set(best.source ?? '', (counts.get(best.source ?? '') ?? 0) + 1);
    out.push(best);
  }
  return out;
}

/** A local day as the server's instant spelling: [start, end). */
export function dayWindow(day: string): { after: string; before: string } {
  const [y, m, d] = day.split('-').map(Number);
  const spell = (date: Date) => date.toISOString().replace(/\.\d{3}Z$/, '+00:00');
  return { after: spell(new Date(y, m - 1, d)), before: spell(new Date(y, m - 1, d + 1)) };
}

const DAYS = 14;
const PAGE = 1000;

/** Line by line through a day: each plays by itself, and a person either fixes
 * the words or says they are right. Either way the check is filed as words heard
 * and vouched for, which is what scoring the transcripts needs (#1461). */
@Component({
  selector: 'app-check',
  imports: [
    TextFieldModule,
    MatButtonModule,
    MatCardModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatProgressBarModule,
    MatSelectModule,
  ],
  templateUrl: './check.html',
  styleUrl: './check.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Check {
  private readonly api = inject(RecallApi);
  private readonly http = inject(HttpClient);
  private readonly snack = inject(MatSnackBar);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  protected readonly player = inject(Player);
  protected readonly clock = timeOfDaySeconds;

  /** `YYYY-MM-DD`, local. */
  readonly day = input('', { transform: (v: string | undefined) => v ?? '' });
  /** Comma-separated line ids, from a link to one line. */
  readonly ids = input('', { transform: (v: string | undefined) => v ?? '' });

  protected readonly days = Array.from({ length: DAYS }, (_, i) => {
    const d = new Date();
    d.setDate(d.getDate() - i);
    const key = dayKey(d.toISOString());
    const label =
      i === 0
        ? 'Today'
        : d.toLocaleDateString(undefined, { weekday: 'long', day: 'numeric', month: 'long' });
    return { key, label };
  });

  protected readonly lines = signal<Transcript[]>([]);
  protected readonly at = signal(0);
  protected readonly loading = signal(false);
  protected readonly failed = signal(false);
  protected readonly busy = signal(false);
  /** Line id to the words filed for it, this visit. */
  protected readonly checked = signal<ReadonlyMap<number, string>>(new Map());

  protected readonly current = computed(() => this.lines()[this.at()] ?? null);
  protected readonly before = computed(() => this.lines()[this.at() - 1] ?? null);
  protected readonly after = computed(() => this.lines()[this.at() + 1] ?? null);
  /** The words being filed; each line starts from its own. */
  protected readonly draft = linkedSignal(() => this.current()?.text ?? '');
  protected readonly dirty = computed(
    () => this.draft().trim() !== (this.current()?.text ?? '').trim(),
  );
  protected readonly progress = computed(() =>
    this.lines().length ? (100 * this.at()) / this.lines().length : 0,
  );

  constructor() {
    effect(() => {
      const day = this.day();
      const ids = this.ids();
      untracked(() => void this.load(day, ids));
    });
    // A new line plays by itself.
    effect(() => {
      const t = this.current();
      untracked(() => {
        if (t && !this.checked().has(t.id)) this.play(t, true);
      });
    });
  }

  protected pickDay(day: string | undefined): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { day: day ?? null, ids: null },
    });
  }

  private async load(day: string, ids: string): Promise<void> {
    this.lines.set([]);
    this.at.set(0);
    this.failed.set(false);
    if (!day && !ids) return;
    this.loading.set(true);
    try {
      this.lines.set(ids ? await this.byIds(ids) : await this.ofDay(day));
    } catch {
      this.failed.set(true);
    } finally {
      this.loading.set(false);
    }
  }

  private async byIds(ids: string): Promise<Transcript[]> {
    const url = `/api/transcripts?ids=${encodeURIComponent(ids)}`;
    const list = await firstValueFrom(this.http.get<TranscriptList>(url));
    return list.items.filter((t) => t.tier !== 'live');
  }

  private async ofDay(day: string): Promise<Transcript[]> {
    const { after, before } = dayWindow(day);
    const moments: Moment[] = [];
    let cursor = after;
    for (;;) {
      const page = await firstValueFrom(this.api.conversations(PAGE, before, cursor));
      for (const c of page.items) moments.push(...c.moments);
      const last = page.items.at(-1);
      if (!page.hasMore || !last || last.end <= cursor) break;
      cursor = last.end;
    }
    return pickLines(moments);
  }

  protected play(t: Transcript, fresh = false): void {
    const key = `clip:${t.audioUrl}`;
    if (fresh && this.player.playing() === key) return;
    this.player.toggle(key, this.player.clip(t.audioUrl));
  }

  protected playing(t: Transcript): boolean {
    return this.player.playing() === `clip:${t.audioUrl}`;
  }

  /** File the words as heard: fixed, or confirmed unchanged. Enter does it;
   * Shift+Enter, matching no `keydown.enter`, stays a newline. */
  protected confirm(): void {
    const t = this.current();
    const text = this.draft().trim();
    if (!t || !text || this.busy() || this.checked().has(t.id)) return;
    this.busy.set(true);
    this.api.correct(t.id, text, { checked: true }).subscribe({
      next: () => {
        this.busy.set(false);
        this.checked.update((m) => new Map(m).set(t.id, text));
        this.next();
      },
      error: () => {
        this.busy.set(false);
        this.snack.open('Could not save, try again', 'OK', { duration: 4000 });
      },
    });
  }

  protected next(): void {
    this.at.update((i) => Math.min(i + 1, this.lines().length));
  }

  protected back(): void {
    this.at.update((i) => Math.max(i - 1, 0));
  }
}
