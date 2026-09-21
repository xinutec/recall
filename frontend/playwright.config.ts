import { defineConfig, devices } from '@playwright/test';
import { phoneConfig } from '@xinutec/ui-harness/config';
import harness from './e2e/harness.mjs';

/**
 * Phone-width layout harness. The recall web app is used on the Pixel 9, so the
 * suite runs at its real size to catch controls clipped or hidden behind the
 * fixed bottom nav — geometry the jsdom unit tests cannot see.
 *
 * Everything shared — the Pixel geometry, the port, the static server — comes
 * from @xinutec/ui-harness. This app used to spell out its own device
 * descriptor; it now takes the fleet's, which is the same 412 CSS px at
 * deviceScaleFactor 1 (CSS-pixel geometry is DPR-invariant, and forcing 1 keeps
 * measurements small).
 *
 * The BUILT bundle is served, not `ng serve`: the tests mock every /api call so
 * no backend is involved, and serving the built dist dodges the macOS
 * kqueue.c:279 abort that spawning the CLI dev server trips. `npm run ui-check`
 * builds first; reuseExistingServer attaches to a server you started yourself.
 */
const base = phoneConfig(harness, devices);

/**
 * ⚠ **The service worker is blocked, and the harness does it now.** This suite
 * serves the BUILT bundle, so as of the ngsw adoption a real worker registers —
 * and Playwright's `page.route` does not intercept requests that pass through
 * one. Two `session-assign` tests went to `Received: null` where they expected
 * a captured request, which reads as the app not making the call and sends you
 * into the app. Measured: 2 failed with the worker, 2 passed with it stashed,
 * 2 passed with the block.
 *
 * That measurement is why `phoneConfig` carries `serviceWorkers: "block"` since
 * ui-harness `fd31cb2` (#1625, dev-lint#1384) — every adopter that route-mocks
 * its API needs it, and the others passed only on what their assertions happen
 * to check. The local override that lived here was a second copy of a decision
 * the harness owns; it went when this frontend was bumped to c57aca7.
 *
 * ⚠ What it still gives up: this suite does not exercise the worker at all. The
 * update policy is unit-tested in ui-harness against a fake and the adapter
 * here is thin, so that is a fair trade — but nothing asserts a real worker
 * serves the shell offline.
 */
export default defineConfig(base);
