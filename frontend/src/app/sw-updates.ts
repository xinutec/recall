import { Injectable, inject } from '@angular/core';
import { SwUpdate, VersionReadyEvent } from '@angular/service-worker';
import {
  type PagePort,
  type ServiceWorkerPort,
  SwUpdates as SwUpdatePolicy,
  type UpdateOutcome,
} from '@xinutec/ui-harness/sw-updates';
import { filter } from 'rxjs';

export type { UpdateOutcome };

/** Marks that we have already auto-reloaded out of an unrecoverable service worker
 *  state. Session-scoped so it survives that very reload. Unchanged from when this
 *  logic lived here, so a tab mid-recovery across the upgrade still sees its mark. */
const RECOVERY_KEY = 'recall.sw-recovery-attempted';

/**
 * Self-update — the Angular wiring. The rules live in
 * `@xinutec/ui-harness/sw-updates`; this is the adapter.
 *
 * This app is opened from a phone, so the shell is cached and it loads without
 * waiting for the network. ⚠ **ngsw alone would then cache a build that never
 * learns a newer one exists**, which is worse than no caching because it looks
 * fine. That is why the update path arrives in the same change (dev-lint#1384).
 *
 * ⚠ **Only the SHELL is cached — `ngsw-config.json` carries no dataGroups, and
 * this app is the strongest case for that.** It is a control surface over a LIVE
 * pipeline: a cached `/api/capture` would show recording as running when it had
 * stopped, or the reverse. Whether the mics are listening is the one thing this
 * UI exists to answer, and an answer from cache is not an answer. Offline it
 * opens and shows nothing, which is honest.
 *
 * The policy is shared and unit-tested against a fake; what is here is the
 * Angular wiring, which is the part a unit test cannot reach.
 */
@Injectable({ providedIn: 'root' })
export class SwUpdates {
  private readonly sw = inject(SwUpdate);

  private readonly serviceWorker: ServiceWorkerPort = ((sw: SwUpdate) => ({
    // Bound to a local, not `this`: an object-literal getter does not capture the
    // enclosing `this` lexically, and a copied boolean would freeze `isEnabled` at
    // construction when start() must read the live value.
    get isEnabled(): boolean {
      return sw.isEnabled;
    },
    onVersionReady: (handler: () => void): void => {
      sw.versionUpdates
        .pipe(filter((event): event is VersionReadyEvent => event.type === 'VERSION_READY'))
        .subscribe(() => handler());
    },
    onUnrecoverable: (handler: () => void): void => {
      // The cached build is broken and the server no longer holds the files to repair
      // it — what a roll-forward deploy of :latest leaves a client whose cache was
      // evicted meanwhile. Nothing recovers from here except a fresh load.
      sw.unrecoverable.subscribe(() => handler());
    },
    checkForUpdate: () => sw.checkForUpdate(),
    activateUpdate: () => sw.activateUpdate(),
  }))(this.sw);

  private readonly page: PagePort = {
    get hidden(): boolean {
      return document.visibilityState === 'hidden';
    },
    onVisibilityChange: (handler: () => void): void => {
      document.addEventListener('visibilitychange', handler);
    },
    recoveryAttempted: () => sessionStorage.getItem(RECOVERY_KEY) !== null,
    markRecoveryAttempted: () => sessionStorage.setItem(RECOVERY_KEY, '1'),
    // Routed through the method below rather than called directly, so a test can
    // assert "this would have reloaded" without navigating the test runner.
    reload: () => this.reload(),
    now: () => Date.now(),
  };

  private readonly policy = new SwUpdatePolicy(this.serviceWorker, this.page);

  start(): void {
    this.policy.start();
  }

  /** Manual "Check for updates". Never rejects — every failure comes back as
   *  `'failed'` so the caller can say so. */
  checkNow(): Promise<UpdateOutcome> {
    return this.policy.checkNow();
  }

  /** The one place the page is thrown away. Its own method so tests can assert
   *  "this would have reloaded" without navigating the test runner. */
  reload(): void {
    document.location.reload();
  }
}
