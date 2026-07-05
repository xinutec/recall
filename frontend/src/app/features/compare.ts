import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  inject,
  signal,
} from '@angular/core';
import { httpResource } from '@angular/common/http';
import { Router, RouterLink } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { MatAutocompleteModule } from '@angular/material/autocomplete';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatExpansionModule } from '@angular/material/expansion';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatIconModule } from '@angular/material/icon';
import { MatInputModule } from '@angular/material/input';
import { MatProgressBarModule } from '@angular/material/progress-bar';

import { AbCompareRunList, SessionList } from '../models';
import { RecallApi } from '../recall-api';
import { verdictOf, werPct } from '../ab-compare-format';

const LIST_POLL_MS = 5_000;

/** A/B model comparisons: start a new one (defaults to the deployed live-vs-adapter
 * pairing, so it's one field + Run), and browse past runs with their verdict. Click a
 * run to open its evidence. */
@Component({
  selector: 'app-compare',
  imports: [
    RouterLink,
    FormsModule,
    MatAutocompleteModule,
    MatButtonModule,
    MatCardModule,
    MatExpansionModule,
    MatFormFieldModule,
    MatIconModule,
    MatInputModule,
    MatProgressBarModule,
  ],
  templateUrl: './compare.html',
  styleUrl: './compare.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Compare {
  private readonly api = inject(RecallApi);
  private readonly router = inject(Router);

  protected readonly data = httpResource<AbCompareRunList>(() => '/api/ab-compare');
  protected readonly runs = computed(() => this.data.value()?.items ?? []);
  protected readonly empty = computed(() => !this.runs().length && !this.data.isLoading());

  // Source suggestions: the uploaded recordings (the usual A/B targets). The user can
  // also type a live source id (usb, pixel9, …) the datalist doesn't list.
  private readonly sessions = httpResource<SessionList>(() => '/api/sessions');
  protected readonly sourceOptions = computed(() => this.sessions.value()?.items ?? []);

  // The autocomplete list, filtered by what's typed (id or title). Empty query → all,
  // so focusing the field shows every recording; typing a live source id (usb, pixel9)
  // that isn't a session still works — the field is free text.
  protected readonly filteredSources = computed(() => {
    const q = this.source().trim().toLowerCase();
    const all = this.sourceOptions();
    if (!q) return all;
    return all.filter(
      (s) => s.id.toLowerCase().includes(q) || s.title.toLowerCase().includes(q),
    );
  });

  protected readonly source = signal('');
  protected readonly from = signal('');
  protected readonly to = signal('');
  protected readonly modelA = signal('');
  protected readonly modelB = signal('');
  protected readonly baseModel = signal('');
  protected readonly starting = signal(false);
  protected readonly error = signal('');

  protected readonly canStart = computed(() => !!this.source().trim() && !this.starting());

  constructor() {
    // While any run is queued/running, refresh the list so its status (and verdict)
    // updates without a manual reload. Cleared on destroy — a leaked interval would
    // retain the dead component and keep polling for the tab's lifetime.
    const poller = setInterval(() => {
      if (this.runs().some((r) => r.status === 'queued' || r.status === 'running')) {
        this.data.reload();
      }
    }, LIST_POLL_MS);
    inject(DestroyRef).onDestroy(() => clearInterval(poller));
  }

  protected start(): void {
    const source = this.source().trim();
    if (!source || this.starting()) return;
    this.starting.set(true);
    this.error.set('');
    const body = {
      source,
      ...(this.from().trim() ? { from: this.from().trim() } : {}),
      ...(this.to().trim() ? { to: this.to().trim() } : {}),
      ...(this.modelA().trim() ? { modelA: this.modelA().trim() } : {}),
      ...(this.modelB().trim() ? { modelB: this.modelB().trim() } : {}),
      ...(this.baseModel().trim() ? { baseModel: this.baseModel().trim() } : {}),
    };
    this.api.startAbCompare(body).subscribe({
      next: (r) => {
        this.starting.set(false);
        void this.router.navigate(['/compare', r.newId]);
      },
      error: () => {
        this.starting.set(false);
        this.error.set('Could not start the comparison — is the backend running?');
      },
    });
  }

  protected readonly verdict = verdictOf;
  protected readonly pct = werPct;
}
