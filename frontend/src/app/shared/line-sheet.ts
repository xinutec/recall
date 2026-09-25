import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { RouterLink } from '@angular/router';
import { Observable } from 'rxjs';
import { TextFieldModule } from '@angular/cdk/text-field';
import { MatAutocompleteModule } from '@angular/material/autocomplete';
import { MAT_BOTTOM_SHEET_DATA, MatBottomSheetRef } from '@angular/material/bottom-sheet';
import { MatButtonModule } from '@angular/material/button';
import { MatChipsModule } from '@angular/material/chips';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatListModule } from '@angular/material/list';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Transcript } from '../models';
import { RecallApi } from '../recall-api';
import { timeOfDaySeconds } from '../format';
import { Player } from './player';

export interface LineSheetData {
  readonly turn: Transcript;
  /** Who the line is shown as, and whether a person said so. */
  readonly speaker: string;
  readonly confirmed: boolean;
  /** Names offered as one tap. */
  readonly names: readonly string[];
  /** Names the field suggests. */
  readonly known: readonly string[];
  /** The same moment as other mics heard it. */
  readonly alternates: readonly Transcript[];
}

export interface Word {
  readonly text: string;
  /** Character offsets, end exclusive. Characters, not UTF-16 units: the server counts `char`s. */
  readonly start: number;
  readonly end: number;
}

export function wordsOf(text: string): Word[] {
  const chars = Array.from(text);
  const out: Word[] = [];
  let start = -1;
  for (let i = 0; i <= chars.length; i++) {
    const space = i === chars.length || /\s/.test(chars[i]);
    if (space && start >= 0) {
      out.push({ text: chars.slice(start, i).join(''), start, end: i });
      start = -1;
    } else if (!space && start < 0) {
      start = i;
    }
  }
  return out;
}

type Mode = 'main' | 'part' | 'edit' | 'mics';

/** Everything to do with one line: hear it, say who said it, give part of it to
 * someone else, fix its words, compare the mics. Closes with `true` after a write. */
@Component({
  selector: 'app-line-sheet',
  imports: [
    NgTemplateOutlet,
    RouterLink,
    TextFieldModule,
    MatAutocompleteModule,
    MatButtonModule,
    MatChipsModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatListModule,
  ],
  templateUrl: './line-sheet.html',
  styleUrl: './line-sheet.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class LineSheet {
  protected readonly data = inject<LineSheetData>(MAT_BOTTOM_SHEET_DATA);
  private readonly ref = inject<MatBottomSheetRef<LineSheet, boolean>>(MatBottomSheetRef);
  private readonly api = inject(RecallApi);
  private readonly snack = inject(MatSnackBar);
  protected readonly player = inject(Player);
  protected readonly clock = timeOfDaySeconds;

  protected readonly t = this.data.turn;
  /** A live line is minutes from being replaced by its transcription. */
  protected readonly editable = this.t.tier !== 'live';
  protected readonly mode = signal<Mode>('main');
  protected readonly busy = signal(false);

  protected readonly typed = signal('');
  protected readonly suggestions = computed(() => {
    const q = this.typed().trim().toLowerCase();
    return this.data.known.filter((n) => n.toLowerCase().includes(q));
  });

  protected play(t: Transcript = this.t): void {
    this.player.toggle(`turn:${t.id}`, this.player.clip(t.audioUrl));
  }

  protected playing(t: Transcript = this.t): boolean {
    return this.player.playing() === `turn:${t.id}`;
  }

  protected open(mode: Mode): void {
    this.typed.set('');
    this.from.set(null);
    this.to.set(null);
    this.mode.set(mode);
  }

  // --- who said it

  /** A name from the chips or the field: for the whole line, or the picked part. */
  protected choose(name: string | undefined): void {
    if (!name) return;
    if (this.mode() === 'part') this.givePart(name);
    else this.sayWho(name);
  }

  /** A correction, so it also enrols the voice. */
  protected sayWho(name: string): void {
    const who = name.trim();
    if (!who || (who === this.data.speaker && this.data.confirmed)) return;
    this.write(this.api.correct(this.t.id, this.t.text, { speaker: who }));
  }

  // --- part of the line

  protected readonly words = wordsOf(this.t.text);
  protected readonly from = signal<number | null>(null);
  protected readonly to = signal<number | null>(null);

  /** The first tap marks a word, the second closes the range, a third starts over. */
  protected pick(i: number): void {
    const from = this.from();
    if (from === null || this.to() !== null) {
      this.from.set(i);
      this.to.set(null);
    } else {
      this.from.set(Math.min(from, i));
      this.to.set(Math.max(from, i));
    }
  }

  protected inPart(i: number): boolean {
    const from = this.from();
    if (from === null) return false;
    return i >= from && i <= (this.to() ?? from);
  }

  protected readonly partText = computed(() => {
    const from = this.from();
    if (from === null) return '';
    return this.words
      .slice(from, (this.to() ?? from) + 1)
      .map((w) => w.text)
      .join(' ');
  });

  protected givePart(name: string): void {
    const from = this.from();
    const who = name.trim();
    if (from === null || !who || !this.t.source) return;
    const last = this.words[this.to() ?? from];
    this.write(
      this.api.assignSpan(this.t.source, {
        startTurn: this.t.id,
        startChar: this.words[from].start,
        endTurn: this.t.id,
        endChar: last.end,
        name: who,
      }),
    );
  }

  // --- words

  protected saveText(text: string): void {
    const fixed = text.trim();
    if (!fixed || fixed === this.t.text) return;
    this.write(this.api.correct(this.t.id, fixed));
  }

  protected copy(): void {
    void navigator.clipboard?.writeText(this.t.text);
    this.snack.open('Copied', undefined, { duration: 2000 });
  }

  // --- writing

  /** One write at a time, so a double tap can't file it twice. */
  private write(call: Observable<unknown>): void {
    if (this.busy()) return;
    this.busy.set(true);
    call.subscribe({
      next: () => this.ref.dismiss(true),
      error: () => {
        this.busy.set(false);
        this.snack.open('Could not save, try again', 'OK', { duration: 4000 });
      },
    });
  }
}
