import { ErrorHandler, Injectable } from '@angular/core';
import { HttpInterceptorFn } from '@angular/common/http';
import { catchError, throwError } from 'rxjs';

import { stringField } from './narrow';

/**
 * Send a browser error to the server's `logs/client.log`, since a phone's
 * console cannot be read. With `fetch`, so it does not pass through the
 * interceptors.
 */
export function reportToServer(level: string, message: string, stack?: string): void {
  try {
    void fetch('/api/log', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ level, message, stack, url: location.href }),
      // dev-lint: allow-ignored-error the error report itself failed; there is nowhere left to say so
    }).catch(() => undefined);
  } catch {
    /* dropped */
  }
}

@Injectable()
export class ServerErrorHandler implements ErrorHandler {
  handleError(error: unknown): void {
    // Anything can be thrown; `String(error)` on a plain object would log
    // "[object Object]".
    reportToServer(
      'error',
      stringField(error, 'message') ?? (typeof error === 'string' ? error : 'non-Error thrown'),
      stringField(error, 'stack') ?? undefined,
    );
    console.error(error);
  }
}

/** Reports failed API calls to the server. */
export const serverLogInterceptor: HttpInterceptorFn = (req, next) =>
  next(req).pipe(
    catchError((err: { status?: number; statusText?: string; message?: string }) => {
      if (!req.url.includes('/api/log')) {
        reportToServer(
          'http',
          `${req.method} ${req.url} -> ${err.status ?? '?'} ${err.statusText ?? err.message ?? ''}`,
        );
      }
      return throwError(() => err);
    }),
  );
