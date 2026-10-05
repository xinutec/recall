import { defineConfig, devices } from '@playwright/test';
import { phoneConfig } from '@xinutec/ui-harness/config';
import harness from './e2e/harness.mjs';

/**
 * The layout suite, at the Pixel's real size (412 CSS px), to catch what jsdom
 * cannot: controls clipped or hidden behind the bottom nav. The geometry, port
 * and static server come from @xinutec/ui-harness.
 *
 * It serves the built bundle, not `ng serve`, which aborts in macOS's kqueue;
 * `npm run ui-check` builds first. Every /api call is mocked.
 *
 * The harness blocks the service worker, since `page.route` cannot see
 * requests that pass through one (#1625). So nothing here checks that a real
 * worker serves the shell offline.
 */
export default defineConfig(phoneConfig(harness, devices));
