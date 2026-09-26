import { expect, test, type Page, type Route } from '@playwright/test';

// Hermetic: every /api call is mocked here, so the e2e touches no real data.

function line(id: number, source: string, text: string): unknown {
  return {
    id,
    start: '2026-09-19T10:00:0' + id + '+00:00',
    end: '2026-09-19T10:00:0' + (id + 1) + '+00:00',
    text,
    language: 'en',
    speaker: 'Pippijn',
    speakerConfirmed: true,
    speakerConfidence: null,
    confidence: 0.9,
    loudness: 0.01,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    audioUrl: `/api/audio/${id}`,
    source,
    cluster: null,
  };
}

const moments = [
  [
    line(1, 'usb', 'I have already made a list of errands.'),
    line(2, 'pixel9', 'I have already made a list of errors.'),
  ],
  [
    line(3, 'usb', 'The pharmacy closes early on Fridays.'),
    line(4, 'pixel9', 'The farmacy closes early on fridays.'),
  ],
  [line(5, 'usb', 'Then the bakery.'), line(6, 'pixel9', 'Then the bakery.')],
].map(([a, b]) => ({
  start: '',
  end: '',
  primary: [a],
  alternates: [b],
  sources: ['usb', 'pixel9'],
}));

async function mockApi(page: Page): Promise<unknown[]> {
  const posted: unknown[] = [];
  await page.route('**/api/**', async (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) {
      return route.fulfill({
        json: {
          items: [
            {
              start: '2026-09-19T10:00:00+00:00',
              end: '2026-09-19T10:00:09+00:00',
              turnCount: 6,
              speakers: [],
              preview: '',
              moments,
            },
          ],
          hasMore: false,
        },
      });
    }
    if (url.includes('/api/correct')) {
      posted.push(route.request().postDataJSON());
      return route.fulfill({ json: { newId: 100 + posted.length } });
    }
    if (url.includes('/api/no-speech')) {
      const body: unknown = route.request().postDataJSON();
      posted.push({ noSpeech: body });
      return route.fulfill({ json: { ok: true } });
    }
    return route.fulfill({ json: {} });
  });
  return posted;
}

test('checks a day line by line, alternating mics (Pixel 9)', async ({ page }) => {
  const posted = await mockApi(page);
  await page.goto('/check');
  await page.getByRole('combobox', { name: 'Day' }).click();
  await page.getByRole('option', { name: 'Today' }).click();

  await expect(page.getByText('Line 1 of 3')).toBeVisible();
  const right = page.getByRole('button', { name: 'Words are right' });
  await expect(right).toBeInViewport();
  await right.click();

  // The second moment is checked on the other mic.
  await expect(page.getByText('Line 2 of 3')).toBeVisible();
  const box = page.getByRole('textbox', { name: 'What was said' });
  await expect(box).toHaveValue('The farmacy closes early on fridays.');
  await box.fill('The pharmacy closes early on Fridays.');
  await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
  await box.press('Enter');

  await expect(page.getByText('Line 3 of 3')).toBeVisible();
  await page.getByRole('button', { name: 'Skip' }).click();
  await expect(page.getByText('2 of 3 lines checked')).toBeVisible();

  expect(posted).toEqual([
    { id: 1, text: 'I have already made a list of errands.', checked: true },
    { id: 4, text: 'The pharmacy closes early on Fridays.', checked: true },
  ]);
});

test('a line nobody spoke is filed as such, not as words (Pixel 9)', async ({ page }) => {
  const posted = await mockApi(page);
  await page.goto('/check');
  await page.getByRole('combobox', { name: 'Day' }).click();
  await page.getByRole('option', { name: 'Today' }).click();

  await expect(page.getByText('Line 1 of 3')).toBeVisible();
  const nobody = page.getByRole('button', { name: 'Nobody spoke' });
  await expect(nobody).toBeInViewport();
  await nobody.click();
  await expect(page.getByText('Line 2 of 3')).toBeVisible();

  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page.getByText('Nobody spoke', { exact: true })).toBeVisible();
  await expect(page.getByText('1 checked')).toBeVisible();

  expect(posted).toEqual([{ noSpeech: { id: 1 } }]);
});
