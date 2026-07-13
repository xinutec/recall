import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { MatDialog } from '@angular/material/dialog';
import { MatSnackBar } from '@angular/material/snack-bar';
import { EMPTY, of, throwError } from 'rxjs';
import { HttpErrorResponse } from '@angular/common/http';
import { expect, it, vi } from 'vitest';

import { Cleanup } from './cleanup';
import { RecallApi } from '../recall-api';
import { QuietSpan } from '../models';

const SPAN: QuietSpan = {
  source: 'usb',
  start: '2026-06-14T21:10:11Z',
  end: '2026-06-15T07:44:11Z',
  durationS: 38040,
  audioIds: [1, 2, 3, 4, 5, 6],
  soundSeconds: 22.8,
  loudestDb: -39,
  marginDb: 15,
  silent: false,
  structure: 0.6,
};

/** The page, with its network stubbed and a dialog that answers however a test says. */
function setup(answer: boolean | undefined, deleteResult = of({ deleted: 6, freedBytes: 6e6 })) {
  const quietDelete = vi.fn().mockReturnValue(deleteResult);
  const open = vi.fn().mockReturnValue({ afterClosed: () => of(answer) });
  const snack = { open: vi.fn() };

  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      {
        provide: RecallApi,
        useValue: {
          quietSpans: () => of({ items: [SPAN] }),
          quietScanProgress: () => EMPTY,
          quietDelete,
        },
      },
      { provide: MatDialog, useValue: { open } },
      { provide: MatSnackBar, useValue: snack },
    ],
  });
  const fixture = TestBed.createComponent(Cleanup);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const page = fixture.componentInstance as any;
  return { page, quietDelete, open, snack };
}

it('asks before deleting, and deletes what the span holds when told to', () => {
  const { page, quietDelete, open } = setup(true);

  page.delete(SPAN);

  expect(open).toHaveBeenCalledOnce(); // it asked...
  expect(quietDelete).toHaveBeenCalledWith({ audioIds: [1, 2, 3, 4, 5, 6] }); // ...then acted
});

it('deletes nothing when the confirmation is declined', () => {
  const { page, quietDelete } = setup(false);

  page.delete(SPAN);

  expect(quietDelete).not.toHaveBeenCalled();
});

it('never uses window.confirm — inside the Android WebView it returns false undrawn', () => {
  // The bug this file exists for. The app runs in a WebView with no WebChromeClient, and
  // such a WebView returns false from confirm() *without ever drawing a dialog*: the
  // delete silently did nothing, every time, and there was no way for it to say so. If
  // anyone reaches for window.confirm again, this fails.
  const confirm = vi.fn().mockReturnValue(true);
  vi.stubGlobal('confirm', confirm);
  const { page, quietDelete } = setup(true);

  page.delete(SPAN);

  expect(confirm).not.toHaveBeenCalled();
  expect(quietDelete).toHaveBeenCalledOnce(); // and it still went through
  vi.unstubAllGlobals();
});

it('says so when a delete fails, instead of looking like it worked', () => {
  const failed = throwError(() => new HttpErrorResponse({ status: 500 }));
  const { page, snack } = setup(true, failed);

  page.delete(SPAN);

  const [message] = snack.open.mock.calls[0] as [string];
  expect(message).toContain('failed');
  expect(message).toContain('nothing was removed');
});
