import { ChangeDetectionStrategy, Component, computed, inject, signal } from '@angular/core';
import { httpResource } from '@angular/common/http';
import { Router } from '@angular/router';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatChipsModule } from '@angular/material/chips';
import { MatIconModule } from '@angular/material/icon';
import { MatMenuModule } from '@angular/material/menu';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Session, SessionList } from '../models';
import { RecallApi } from '../recall-api';
import { dayLabel, timeOfDay } from '../format';

/** Uploaded recordings, such as appointments: upload, open, rename, re-diarize, delete. */
@Component({
  selector: 'app-sessions',
  imports: [
    MatCardModule,
    MatButtonModule,
    MatChipsModule,
    MatIconModule,
    MatMenuModule,
    MatFormFieldModule,
    MatInputModule,
    MatProgressBarModule,
  ],
  templateUrl: './sessions.html',
  styleUrl: './sessions.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Sessions {
  private readonly api = inject(RecallApi);
  private readonly router = inject(Router);
  private readonly snack = inject(MatSnackBar);

  protected readonly data = httpResource<SessionList>(() => '/api/sessions');
  protected readonly items = computed(() => this.data.value()?.items ?? []);
  protected readonly empty = computed(() => !this.items().length && !this.data.isLoading());

  protected readonly uploading = signal(false);
  protected readonly editingId = signal<string | null>(null);
  protected readonly editTitle = signal('');
  // Delete takes two taps.
  protected readonly confirmingId = signal<string | null>(null);

  protected readonly day = dayLabel;
  protected readonly time = timeOfDay;

  protected open(id: string): void {
    void this.router.navigate(['/sessions', id]);
  }

  /** Upload the picked file, dated by its last-modified time: when it was recorded. */
  protected onFile(input: HTMLInputElement): void {
    const file = input.files?.[0];
    input.value = ''; // let the same file be re-picked after an error
    if (!file) {
      return;
    }
    this.uploading.set(true);
    const start = new Date(file.lastModified).toISOString();
    this.api.createSession(file, '', start).subscribe({
      next: (s: Session) => {
        this.uploading.set(false);
        this.data.reload();
        this.snack.open(`Uploaded “${s.title}” — transcribing…`, undefined, { duration: 4000 });
      },
      error: () => {
        this.uploading.set(false);
        this.snack.open('Upload failed', 'Dismiss', { duration: 5000 });
      },
    });
  }

  protected startEdit(item: Session): void {
    this.confirmingId.set(null);
    this.editingId.set(item.id);
    this.editTitle.set(item.title);
  }

  protected cancelEdit(): void {
    this.editingId.set(null);
  }

  protected saveEdit(id: string): void {
    const title = this.editTitle().trim();
    if (!title) {
      return;
    }
    this.api.renameSession(id, title).subscribe({
      next: () => {
        this.editingId.set(null);
        this.data.reload();
      },
      error: () => this.snack.open('Could not rename', 'Dismiss', { duration: 4000 }),
    });
  }

  protected rediarize(id: string): void {
    this.api.rediarizeSession(id).subscribe({
      next: () =>
        this.snack.open('Re-diarize queued — runs while recording is paused', undefined, {
          duration: 4000,
        }),
      error: () => this.snack.open('Could not queue re-diarize', 'Dismiss', { duration: 4000 }),
    });
  }

  protected askDelete(id: string): void {
    this.editingId.set(null);
    this.confirmingId.set(id);
  }

  protected cancelDelete(): void {
    this.confirmingId.set(null);
  }

  protected confirmDelete(id: string): void {
    this.api.deleteSession(id).subscribe({
      next: () => {
        this.confirmingId.set(null);
        this.data.reload();
        this.snack.open('Session deleted', undefined, { duration: 2500 });
      },
      error: () => this.snack.open('Could not delete', 'Dismiss', { duration: 4000 }),
    });
  }
}
