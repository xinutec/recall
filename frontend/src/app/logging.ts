import { ErrorHandler, Injectable } from '@angular/core';
import { HttpInterceptorFn } from '@angular/common/http';
import { catchError, throwError } from 'rxjs';

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
    const e = error as { message?: string; stack?: string };
    reportToServer('error', e?.message ?? String(error), e?.stack);
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
