import { Injectable, inject } from '@angular/core';
import { SwUpdate, VersionReadyEvent } from '@angular/service-worker';
import {
  type PagePort,
  type ServiceWorkerPort,
  SwUpdates as SwUpdatePolicy,
} from '@xinutec/ui-harness/sw-updates';
import { filter } from 'rxjs';

/** Set once the page has reloaded out of an unrecoverable service worker state;
 *  session-scoped, so it survives that reload. */
const RECOVERY_KEY = 'recall.sw-recovery-attempted';

/**
 * Self-update: the Angular side of `@xinutec/ui-harness/sw-updates`, which
 * holds the rules. A cached shell without an update path would never learn of
 * a newer build (dev-lint#1384).
 *
 * Only the shell is cached; `ngsw-config.json` has no dataGroups. A cached
 * `/api/capture` could show the mics recording when they had stopped, so
 * offline the app opens and shows nothing.
 */
@Injectable({ providedIn: 'root' })
export class SwUpdates {
  private readonly sw = inject(SwUpdate);

  private readonly serviceWorker: ServiceWorkerPort = ((sw: SwUpdate) => ({
    // A getter on `sw`, not `this` (an object-literal getter does not capture
    // the enclosing `this`), and not a copy, which would freeze the value.
    get isEnabled(): boolean {
      return sw.isEnabled;
    },
    onVersionReady: (handler: () => void): void => {
      sw.versionUpdates
        .pipe(filter((event): event is VersionReadyEvent => event.type === 'VERSION_READY'))
        .subscribe(() => handler());
    },
    onUnrecoverable: (handler: () => void): void => {
      // The cached build is broken and the server no longer has its files, as
      // after a deploy while the cache was partly evicted. Only a reload helps.
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
    reload: () => this.reload(),
    now: () => Date.now(),
  };

  private readonly policy = new SwUpdatePolicy(this.serviceWorker, this.page);

  start(): void {
    this.policy.start();
  }

  /** A method so a test can stub it instead of reloading the runner. */
  reload(): void {
    document.location.reload();
  }
}
