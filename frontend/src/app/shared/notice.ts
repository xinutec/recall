import { MatSnackBar } from '@angular/material/snack-bar';

/** A write that did not go through. One wording of the button everywhere. */
export function failed(snack: MatSnackBar, message: string): void {
  snack.open(message, 'Dismiss', { duration: 4000 });
}
