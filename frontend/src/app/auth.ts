import { HttpInterceptorFn } from '@angular/common/http';
import { Injectable, inject, signal } from '@angular/core';
import { catchError, throwError } from 'rxjs';

/**
 * Sign-in state for the Nextcloud sign-in wall. Inert unless the server gates
 * `/api/*`.
 */
@Injectable({ providedIn: 'root' })
export class AuthState {
  /** Set when an API call first comes back 401. */
  readonly needsSignIn = signal(false);

  /** The server's login route, returning here. A plain href: `/login`
   * redirects out to Nextcloud and back. */
  loginUrl(): string {
    const returnTo = location.pathname + location.search;
    return `/login?return_to=${encodeURIComponent(returnTo)}`;
  }
}

/** Raises the sign-in wall on a 401 from `/api/`. */
export const authInterceptor: HttpInterceptorFn = (req, next) => {
  const auth = inject(AuthState);
  return next(req).pipe(
    catchError((err: { status?: number }) => {
      if (err.status === 401 && req.url.includes('/api/')) {
        auth.needsSignIn.set(true);
      }
      return throwError(() => err);
    }),
  );
};
