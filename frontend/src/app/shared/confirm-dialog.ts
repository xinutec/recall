import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MAT_DIALOG_DATA, MatDialogModule, MatDialogRef } from '@angular/material/dialog';

/** What a confirmation asks, and what the button that goes through with it says. */
export interface ConfirmData {
  readonly title: string;
  readonly message: string;
  readonly confirm: string;
  /** Paints the confirming button as destructive. For anything irreversible. */
  readonly destructive?: boolean;
}

/**
 * Ask before something irreversible. Replaces `window.confirm`, which this app cannot
 * use: it runs inside an Android WebView, and a WebView with no `WebChromeClient`
 * *silently returns false* from `confirm()` — no dialog is drawn, and the caller reads
 * it as "the user said no". The Delete button on the cleanup page did nothing at all in
 * the phone app for exactly that reason, and said nothing while doing it. A dialog the
 * app draws itself cannot be silently absent.
 */
@Component({
  selector: 'app-confirm-dialog',
  imports: [MatButtonModule, MatDialogModule],
  templateUrl: './confirm-dialog.html',
  styleUrl: './confirm-dialog.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ConfirmDialog {
  protected readonly data = inject<ConfirmData>(MAT_DIALOG_DATA);
  private readonly ref = inject(MatDialogRef<ConfirmDialog, boolean>);

  protected cancel(): void {
    this.ref.close(false);
  }

  protected confirm(): void {
    this.ref.close(true);
  }
}
