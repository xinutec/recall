import { ErrorHandler, Injectable } from '@angular/core';
import { HttpInterceptorFn } from '@angular/common/http';
import { catchError, throwError } from 'rxjs';

import { stringField } from './narrow';

/**
 * The phone has no console you can read, so browser errors are POSTed to the
 * server (logs/client.log). Uses fetch directly so logging never re-enters the
 * HttpClient interceptor chain.
 */
export function reportToServer(level: string, message: string, stack?: string): void {
  try {
    void fetch('/api/log', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ level, message, stack, url: location.href }),
    }).catch(() => undefined);
  } catch {
    /* never let logging throw */
  }
}

@Injectable()
export class ServerErrorHandler implements ErrorHandler {
  handleError(error: unknown): void {
    // Read, don't assert: an ErrorHandler catches literally anything a
    // component threw, and `String(error)` on a plain object reports
    // "[object Object]" to the server — a log line that says nothing at all.
    reportToServer(
      'error',
      stringField(error, 'message') ?? (typeof error === 'string' ? error : 'non-Error thrown'),
      stringField(error, 'stack') ?? undefined,
    );
    console.error(error);
  }
}

/** Report failed API calls (the phone can't show them). */
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
