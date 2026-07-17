import { HttpInterceptorFn } from '@angular/common/http';
import { Injectable, inject, signal } from '@angular/core';
import { catchError, throwError } from 'rxjs';

/**
 * Sign-in state for the Nextcloud SSO wall.
 *
 * Only relevant on the fleet deployment, where the server gates the browsing API
 * (`/api/*`) behind a Nextcloud session. The open LAN UI never returns 401, so
 * `needsSignIn` stays false there and no wall is ever shown — this is inert unless the
 * server has the gate up.
 */
@Injectable({ providedIn: 'root' })
export class AuthState {
  /** Flipped true the first time a gated API call comes back 401. */
  readonly needsSignIn = signal(false);

  /** Full-page link to the server's login route, returning to the current view.
   * A plain href (not routerLink) on purpose: `/login` is a server redirect out to
   * Nextcloud and back, not an in-app route. */
  loginUrl(): string {
    const returnTo = location.pathname + location.search;
    return `/login?return_to=${encodeURIComponent(returnTo)}`;
  }
}

/** Raise the sign-in wall when the server rejects a browsing call as unauthenticated. */
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
