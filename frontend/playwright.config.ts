import { defineConfig } from '@playwright/test';

// Pixel 9 web viewport. The phone's panel is 1080×2424 physical at devicePixelRatio
// 2.625, which is 412×915 CSS px once the status bar is excluded. The recall web app
// is used on the Pixel 9, so e2e runs at its real size to catch controls clipped or
// hidden behind the fixed bottom nav — geometry the jsdom unit tests can't see.
const pixel9 = {
  defaultBrowserType: 'chromium' as const,
  viewport: { width: 412, height: 915 },
  deviceScaleFactor: 2.625,
  isMobile: true,
  hasTouch: true,
  userAgent:
    'Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) ' +
    'Chrome/130.0.0.0 Mobile Safari/537.36',
};

const PORT = 4293;

export default defineConfig({
  testDir: './e2e',
  reporter: 'list',
  use: { baseURL: `http://localhost:${PORT}` },
  projects: [{ name: 'pixel9', use: pixel9 }],
  // Serve the BUILT bundle via a tiny static server (e2e/serve.mjs), not `ng serve`:
  // tests mock every /api call so no backend/real-data is involved, and serving the
  // built dist dodges the macOS kqueue.c:279 abort that spawning the CLI dev server
  // trips. `npm run ui-check` builds first; reuseExistingServer attaches to a
  // serve.mjs you started yourself.
  webServer: {
    command: `node e2e/serve.mjs ${PORT}`,
    url: `http://localhost:${PORT}/`,
    reuseExistingServer: true,
    timeout: 60_000,
  },
});
