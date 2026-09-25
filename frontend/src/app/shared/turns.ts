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
import { MatBottomSheet } from '@angular/material/bottom-sheet';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';

import { Moment, Transcript } from '../models';
import { timeOfDaySeconds } from '../format';
import { LineSheet, LineSheetData } from './line-sheet';
import { Player } from './player';

/** Consecutive turns shown under one speaker. Each turn stays its own tap target. */
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

const COLOURS = ['#8ab4f8', '#fbbc04', '#81c995', '#f28b82', '#c58af9', '#78d9ec', '#ff8bcb'];

/** A stretch of turns as speaker paragraphs. Tapping a line opens its sheet. */
@Component({
  selector: 'app-turns',
  imports: [MatButtonModule, MatIconModule],
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

  private readonly sheet = inject(MatBottomSheet);
  protected readonly player = inject(Player);
  protected readonly clock = timeOfDaySeconds;

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

  /** No speaker separation yet: shown grey, still editable. */
  protected provisional(t: Transcript): boolean {
    return t.tier === 'live' || t.tier === 'transcribed';
  }

  // --- playback

  protected togglePlay(run: Run): void {
    const first = run.turns[0];
    const last = run.turns[run.turns.length - 1];
    const span = `/api/audio-span?from_id=${first.id}&to_id=${last.id}`;
    const each = run.turns.map((t) => this.player.clip(t.audioUrl));
    this.player.toggle(`run:${run.key}`, this.player.clip(span), each);
  }

  ngOnDestroy(): void {
    // Stop only this view's clip: the timeline shows several views at once.
    const id = Number(this.player.playing()?.split(':')[1]);
    const mine = this.moments().some((m) =>
      [...m.primary, ...m.alternates].some((t) => t.id === id),
    );
    if (mine) this.player.stop();
  }

  // --- a line

  /** The line whose sheet is open, marked in the text. */
  protected readonly selected = signal<number | null>(null);

  protected openLine(t: Transcript): void {
    const { name, confirmed } = speakerOf(t, this.voiceNames());
    const known = [...new Set([...this.palette(), ...this.roster()])];
    const data: LineSheetData = {
      turn: t,
      speaker: name,
      confirmed,
      names: this.palette(),
      known,
      alternates: this.alternates(t),
    };
    this.selected.set(t.id);
    this.sheet
      .open<LineSheet, LineSheetData, boolean>(LineSheet, { data })
      .afterDismissed()
      .subscribe((wrote) => {
        this.selected.set(null);
        if (wrote) this.changed.emit();
      });
  }
}
