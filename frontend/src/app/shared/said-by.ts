import { ChangeDetectionStrategy, Component, computed, input, output, signal } from '@angular/core';
import { MatAutocompleteModule } from '@angular/material/autocomplete';
import { MatChipsModule } from '@angular/material/chips';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';

/** Who said a line: one tap on a name in play, or any name typed or suggested. */
@Component({
  selector: 'app-said-by',
  imports: [MatAutocompleteModule, MatChipsModule, MatFormFieldModule, MatInputModule],
  templateUrl: './said-by.html',
  styleUrl: './said-by.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class SaidBy {
  /** Names offered as one tap. */
  readonly names = input<readonly string[]>([]);
  /** Names the field suggests. */
  readonly known = input<readonly string[]>([]);
  /** The name shown as chosen, if any. */
  readonly selected = input<string | null>(null);
  readonly disabled = input(false);
  readonly chosen = output<string>();

  protected readonly typed = signal('');
  protected readonly suggestions = computed(() => {
    const q = this.typed().trim().toLowerCase();
    return this.known().filter((n) => n.toLowerCase().includes(q));
  });

  /** The last name sent since the field was entered: a pick is followed by the
   * panel closing and the field losing focus, and each would send it again. */
  private last = '';

  protected fresh(): void {
    this.last = '';
    this.typed.set('');
  }

  /** A chip. */
  protected choose(name: string | undefined): void {
    const who = name?.trim();
    if (who) this.chosen.emit(who);
  }

  /** The field: on Enter, a suggestion, or leaving it. A typed name counts
   * without Enter, or tapping Save straight after typing loses it. */
  protected take(field: HTMLInputElement, value = field.value): void {
    const who = value.trim();
    field.value = '';
    this.typed.set('');
    if (!who || who === this.last) return;
    this.last = who;
    this.chosen.emit(who);
  }

  /** The panel closed with nothing picked while focus moved elsewhere. */
  protected closed(field: HTMLInputElement): void {
    if (document.activeElement !== field) this.take(field);
  }
}
