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
export default defineConfig(phoneConfig(harness, devices));
