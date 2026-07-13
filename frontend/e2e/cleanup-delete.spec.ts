import { expect, test, type Route } from '@playwright/test';

// Hermetic: every /api call is mocked — no real data, no backend, nothing deleted.

// One span, six segments. The numbers only have to be self-consistent; what is under
// test is whether pressing Delete can reach the server at all.
const span = {
  source: 'usb',
  start: '2026-06-14T21:10:11Z',
  end: '2026-06-14T21:16:11Z',
  durationS: 360,
  audioIds: [1, 2, 3, 4, 5, 6],
  soundSeconds: 0,
  loudestDb: -66,
  marginDb: -12,
  silent: true,
  structure: 0.4,
};

const scan = { running: false, measured: 6, total: 6, analysed: 6, toAnalyse: 6 };

// The panel draws a waveform when opened, so the envelope must be a *real* Envelope —
// a mock missing `points` makes the waveform throw mid-render and takes the panel (and
// the button under test) down with it.
const envelope = {
  start: span.start,
  end: span.end,
  bucketS: 60,
  thresholdDb: -54,
  points: [-66, -66, -66, -66, -66, -66],
  segments: span.audioIds.map((audioId, i) => ({
    audioId,
    start: new Date(Date.parse(span.start) + i * 60_000).toISOString(),
    end: new Date(Date.parse(span.start) + (i + 1) * 60_000).toISOString(),
    meanDb: -66,
  })),
  events: [],
};

/**
 * The bug this exists for: the Delete button did nothing, silently, and said nothing.
 *
 * It called `window.confirm`, and this app runs inside an Android WebView with no
 * WebChromeClient — such a WebView returns false from confirm() *without ever drawing a
 * dialog*, so the code read it as "the user said no" and returned. The unit test cannot
 * catch a regression here: it stubs MatDialog, so a dialog that fails to *open* still
 * passes. Only a real browser running the real bundle can prove the button works.
 */
test('the delete button asks, and then actually deletes (Pixel 9)', async ({ page }) => {
  let deleted: unknown = null;

  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/quiet/spans')) return route.fulfill({ json: { items: [span] } });
    if (url.includes('/api/quiet/scan')) return route.fulfill({ json: scan });
    if (url.includes('/api/quiet/envelope')) return route.fulfill({ json: envelope });
    if (url.includes('/api/quiet/delete')) {
      deleted = route.request().postDataJSON();
      return route.fulfill({ json: { deleted: 6, freedBytes: 6_000_000 } });
    }
    return route.fulfill({ json: {} });
  });

  // If anything reaches for window.confirm again, fail loudly rather than silently
  // taking its answer — in the WebView that answer is always "no", undrawn.
  await page.addInitScript(() => {
    window.confirm = () => {
      throw new Error('window.confirm is unusable in the Android WebView — use ConfirmDialog');
    };
  });

  await page.goto('/cleanup');
  await page.locator('mat-expansion-panel-header').first().click(); // open the span
  await page.locator('button.delete').click();

  // It asks — visibly, in a dialog the app draws itself.
  const dialog = page.locator('mat-dialog-container');
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('cannot be undone');
  expect(deleted).toBeNull(); // and nothing has gone yet

  await dialog.getByRole('button', { name: 'Delete' }).click();

  await expect.poll(() => deleted).toEqual({ audioIds: [1, 2, 3, 4, 5, 6] });
  await expect(page.locator('mat-expansion-panel')).toHaveCount(0); // the span is gone
});

test('declining the dialog deletes nothing (Pixel 9)', async ({ page }) => {
  let deleteCalls = 0;

  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/quiet/spans')) return route.fulfill({ json: { items: [span] } });
    if (url.includes('/api/quiet/scan')) return route.fulfill({ json: scan });
    if (url.includes('/api/quiet/envelope')) return route.fulfill({ json: envelope });
    if (url.includes('/api/quiet/delete')) {
      deleteCalls += 1;
      return route.fulfill({ json: { deleted: 0, freedBytes: 0 } });
    }
    return route.fulfill({ json: {} });
  });

  await page.goto('/cleanup');
  await page.locator('mat-expansion-panel-header').first().click();
  await page.locator('button.delete').click();
  await page.locator('mat-dialog-container').getByRole('button', { name: 'Cancel' }).click();

  await expect(page.locator('mat-dialog-container')).toHaveCount(0);
  expect(deleteCalls).toBe(0);
  await expect(page.locator('mat-expansion-panel')).toHaveCount(1); // still there
});
