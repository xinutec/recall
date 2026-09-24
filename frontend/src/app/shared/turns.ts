import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  OnDestroy,
  output,
  signal,
} from '@angular/core';
import { RouterLink } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { MatButtonModule } from '@angular/material/button';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Moment, Transcript } from '../models';
import { RecallApi } from '../recall-api';
import { resolveSelection, SpanSel } from '../selection-span';
import { timeOfDaySeconds } from '../format';
import { Player } from './player';

/** Consecutive turns shown under one speaker. The turns stay separate spans, so a
 * selection can still split inside the run. */
export interface Run {
  readonly key: number;
  readonly speaker: string;
  readonly confirmed: boolean;
  /** Voice-match strength of an unconfirmed guess, else null. */
  readonly guess: number | null;
  readonly start: string;
  readonly turns: readonly Transcript[];
}

/** Who a turn is shown as: a confirmed name, then a guessed one, then the
 * session's "Voice N" for its cluster. */
export function speakerOf(
  t: Transcript,
  voiceNames: ReadonlyMap<string, string>,
): { name: string; confirmed: boolean } {
  if (t.speaker) return { name: t.speaker, confirmed: t.speakerConfirmed };
  const voice = t.cluster ? voiceNames.get(t.cluster) : undefined;
  return { name: voice ?? 'unknown', confirmed: false };
}

export function runsOf(
  turns: readonly Transcript[],
  voiceNames: ReadonlyMap<string, string> = new Map(),
): Run[] {
  const out: (Omit<Run, 'turns'> & { turns: Transcript[] })[] = [];
  for (const t of turns) {
    const { name, confirmed } = speakerOf(t, voiceNames);
    const last = out.at(-1);
    if (last?.speaker === name && last.confirmed === confirmed) {
      last.turns.push(t);
    } else {
      const guess = !confirmed && t.speaker ? t.speakerConfidence : null;
      out.push({ key: t.id, speaker: name, confirmed, guess, start: t.start, turns: [t] });
    }
  }
  return out;
}

let views = 0;

const COLOURS = ['#8ab4f8', '#fbbc04', '#81c995', '#f28b82', '#c58af9', '#78d9ec', '#ff8bcb'];

/** A stretch of turns as speaker paragraphs, with every way to fix them: say who
 * said a line, move a selected phrase, fix the words, hear it, compare the mics. */
@Component({
  selector: 'app-turns',
  imports: [
    RouterLink,
    FormsModule,
    MatButtonModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
  ],
  templateUrl: './turns.html',
  styleUrl: './turns.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Turns implements OnDestroy {
  readonly moments = input.required<readonly Moment[]>();
  /** Names to offer when nobody in these turns has one yet. */
  readonly roster = input<readonly string[]>([]);
  /** Cluster to "Voice N", for a session's unnamed voices. */
  readonly voiceNames = input<ReadonlyMap<string, string>>(new Map());
  /** A write landed; the parent refetches. */
  readonly changed = output();

  private readonly api = inject(RecallApi);
  private readonly snack = inject(MatSnackBar);
  protected readonly player = inject(Player);
  protected readonly clock = timeOfDaySeconds;
  /** Several views share a page, so the name list needs its own id. */
  protected readonly listId = `names-${++views}`;

  protected readonly turns = computed(() => this.moments().flatMap((m) => m.primary));
  protected readonly runs = computed(() => runsOf(this.turns(), this.voiceNames()));

  private readonly momentOf = computed(() => {
    const out = new Map<number, Moment>();
    for (const m of this.moments()) for (const t of m.primary) out.set(t.id, m);
    return out;
  });

  /** The mic, shown where it changes, and only when more than one is in view. */
  protected readonly sourceTags = computed(() => {
    const out = new Map<number, string>();
    const turns = this.turns();
    if (new Set(turns.map((t) => t.source)).size < 2) return out;
    let last: string | null = null;
    for (const t of turns) {
      if (t.source && t.source !== last) out.set(t.id, t.source);
      last = t.source;
    }
    return out;
  });

  /** Names to offer: everyone named in these turns, else the roster. */
  protected readonly palette = computed(() => {
    const names: string[] = [];
    for (const t of this.turns()) {
      if (t.speaker && !names.includes(t.speaker)) names.push(t.speaker);
    }
    return names.length ? names : [...this.roster()];
  });

  private readonly colours = computed(() => {
    const out = new Map<string, string>();
    for (const run of this.runs()) {
      if (run.speaker !== 'unknown' && !out.has(run.speaker)) {
        out.set(run.speaker, COLOURS[out.size % COLOURS.length]);
      }
    }
    return out;
  });

  protected colourFor(speaker: string): string {
    return this.colours().get(speaker) ?? 'var(--mat-sys-on-surface-variant)';
  }

  protected pct(value: number): string {
    return `${Math.round(value * 100)}%`;
  }

  protected mics(t: Transcript): number {
    return this.momentOf().get(t.id)?.sources.length ?? 1;
  }

  protected alternates(t: Transcript): readonly Transcript[] {
    return this.momentOf().get(t.id)?.alternates ?? [];
  }

  /** Another mic's version names a different speaker for this moment. */
  protected disputed(t: Transcript): boolean {
    return (
      !!t.speaker && this.alternates(t).some((a) => !!a.speaker && a.speaker !== t.speaker)
    );
  }

  /** Live and first-pass turns get replaced by the next pass; an edit would be lost. */
  protected editable(t: Transcript): boolean {
    return t.tier === 'diarized' || t.tier === 'corrected';
  }

  // --- playback

  protected togglePlay(run: Run): void {
    const first = run.turns[0];
    const last = run.turns[run.turns.length - 1];
    const span = `/api/audio-span?from_id=${first.id}&to_id=${last.id}`;
    const each = run.turns.map((t) => this.player.clip(t.audioUrl));
    this.player.toggle(`run:${run.key}`, this.player.clip(span), each);
  }

  protected playTurn(t: Transcript): void {
    this.player.toggle(`turn:${t.id}`, this.player.clip(t.audioUrl));
  }

  ngOnDestroy(): void {
    // Stop only this view's clip: the timeline shows several views at once.
    const id = Number(this.player.playing()?.split(':')[1]);
    const mine = this.moments().some((m) =>
      [...m.primary, ...m.alternates].some((t) => t.id === id),
    );
    if (mine) this.player.stop();
  }

  // --- selecting

  protected readonly selected = signal<number | null>(null);
  protected readonly selectedTurn = computed(
    () => this.turns().find((t) => t.id === this.selected()) ?? null,
  );
  protected readonly comparing = signal(false);

  protected readonly span = signal<SpanSel | null>(null);
  protected readonly spanSource = signal<string | null>(null);
  protected readonly spanText = signal('');

  /** A tap selects a line, a second tap clears it. A drag selection wins. */
  protected selectTurn(id: number): void {
    if (this.span()) return;
    this.comparing.set(false);
    this.selected.update((cur) => (cur === id ? null : id));
  }

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

  protected clearSpan(): void {
    window.getSelection()?.removeAllRanges();
    this.span.set(null);
  }

  // --- writing

  /** Guards every write against a double tap firing it twice. */
  protected readonly busy = signal(false);

  protected assignSpan(name: string): void {
    const span = this.span();
    const source = this.spanSource();
    const who = name.trim();
    if (!span || !source || !who || this.busy()) return;
    this.busy.set(true);
    this.api.assignSpan(source, { ...span, name: who }).subscribe({
      next: () => {
        this.clearSpan();
        this.done();
      },
      error: () => this.fail(),
    });
  }

  /** Say who said the selected line. A correction, so it also enrols the voice. */
  protected assignTurn(name: string): void {
    const t = this.selectedTurn();
    const who = name.trim();
    if (!t || !who || this.busy()) return;
    this.busy.set(true);
    this.api.correct(t.id, t.text, { speaker: who }).subscribe({
      next: () => {
        this.selected.set(null);
        this.done();
      },
      error: () => this.fail(),
    });
  }

  protected readonly editing = signal<number | null>(null);
  protected readonly editingText = computed(
    () => this.turns().find((t) => t.id === this.editing())?.text ?? '',
  );

  protected editText(): void {
    this.editing.set(this.selected());
  }

  protected cancelEdit(): void {
    this.editing.set(null);
  }

  protected saveEdit(text: string): void {
    const id = this.editing();
    this.editing.set(null);
    this.selected.set(null);
    if (id === null || !text.trim() || this.busy()) return;
    this.busy.set(true);
    this.api.correct(id, text.trim()).subscribe({
      next: () => this.done(),
      error: () => this.fail(),
    });
  }

  private done(): void {
    this.busy.set(false);
    this.changed.emit();
  }

  private fail(): void {
    this.busy.set(false);
    this.snack.open('Could not save, try again', 'OK', { duration: 4000 });
  }
}
