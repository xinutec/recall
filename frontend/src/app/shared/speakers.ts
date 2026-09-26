import { Injectable, inject, signal } from '@angular/core';

import { RecallApi } from '../recall-api';

/** Every name recall knows a voice by, held across pages so a revisit never
 * starts from an empty list. `refresh` keeps it current after a naming. */
@Injectable({ providedIn: 'root' })
export class Speakers {
  private readonly api = inject(RecallApi);
  private readonly list = signal<readonly string[]>([]);
  readonly names = this.list.asReadonly();

  refresh(): void {
    this.api.speakers().subscribe({
      next: (r) => this.list.set(r.names),
      // Keeps the last list: a failed refresh is not an empty roster.
      error: () => undefined,
    });
  }
}
