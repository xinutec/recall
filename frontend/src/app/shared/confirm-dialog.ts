import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MAT_DIALOG_DATA, MatDialogModule, MatDialogRef } from '@angular/material/dialog';

export interface ConfirmData {
  readonly title: string;
  readonly message: string;
  /** The confirming button's label. */
  readonly confirm: string;
  /** Styles the confirming button as destructive. */
  readonly destructive?: boolean;
}

/**
 * Ask before something irreversible. Not `window.confirm`: in the Android
 * WebView, without a `WebChromeClient`, it returns false and draws nothing.
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
