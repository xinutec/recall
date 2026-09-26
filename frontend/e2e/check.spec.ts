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
    if (url.includes('/api/correct/undo')) {
      const body: unknown = route.request().postDataJSON();
      posted.push({ undoCheck: body });
      return route.fulfill({ json: { ok: true } });
    }
    if (url.includes('/api/correct')) {
      posted.push(route.request().postDataJSON());
      return route.fulfill({ json: { newId: 100 + posted.length } });
    }
    if (url.includes('/api/speakers')) {
      return route.fulfill({ json: { names: ['Pippijn', 'Dr. Lee'] } });
    }
    if (url.includes('/api/no-speech')) {
      const body: unknown = route.request().postDataJSON();
      posted.push(url.endsWith('/undo') ? { undo: body } : { noSpeech: body });
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
  // The line before now reads as filed, not as the model wrote it.
  await expect(page.locator('.context').first()).toHaveText('Nobody spoke');

  await page.getByRole('button', { name: 'Back' }).click();
  await expect(page.getByText('Line 1 of 3')).toBeVisible();
  await expect(page.locator('.line .words')).toHaveText('Nobody spoke');
  await expect(page.getByText('1 checked')).toBeVisible();

  expect(posted).toEqual([{ noSpeech: { id: 1 } }]);
});

test('a mis-tapped Nobody spoke is undone from the snackbar (Pixel 9)', async ({ page }) => {
  const posted = await mockApi(page);
  await page.goto('/check');
  await page.getByRole('combobox', { name: 'Day' }).click();
  await page.getByRole('option', { name: 'Today' }).click();

  await page.getByRole('button', { name: 'Nobody spoke' }).click();
  await expect(page.getByText('Line 2 of 3')).toBeVisible();
  await page.getByRole('button', { name: 'Undo' }).click();

  // Back on the line, open for checking again.
  await expect(page.getByText('Line 1 of 3')).toBeVisible();
  await expect(page.getByRole('textbox', { name: 'What was said' })).toHaveValue(
    'I have already made a list of errands.',
  );
  await expect(page.getByText('0 checked')).toBeVisible();
  expect(posted).toEqual([{ noSpeech: { id: 1 } }, { undo: { id: 1 } }]);
});

test('the other mics are offered, and a line plays with a margin (Pixel 9)', async ({ page }) => {
  await mockApi(page);
  const audio: string[] = [];
  page.on('request', (r) => {
    if (r.url().includes('/api/audio/')) audio.push(new URL(r.url()).search);
  });
  await page.goto('/check');
  await page.getByRole('combobox', { name: 'Day' }).click();
  await page.getByRole('option', { name: 'Today' }).click();
  await expect(page.getByText('Line 1 of 3')).toBeVisible();

  const other = page.getByRole('button', { name: /I have already made a list of errors/ });
  await expect(other).toBeVisible();
  await other.click();
  await expect(page.getByRole('textbox', { name: 'What was said' })).toHaveValue(
    'I have already made a list of errors.',
  );
  await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
  await expect.poll(() => audio).toContain('?pad=1');
});

test('an accidental Words are right is undone, from the message or later (Pixel 9)', async ({
  page,
}) => {
  const posted = await mockApi(page);
  await page.goto('/check');
  await page.getByRole('combobox', { name: 'Day' }).click();
  await page.getByRole('option', { name: 'Today' }).click();

  await page.getByRole('button', { name: 'Words are right' }).click();
  await expect(page.getByText('Line 2 of 3')).toBeVisible();
  await page.getByRole('button', { name: 'Undo' }).click();
  await expect(page.getByText('Line 1 of 3')).toBeVisible();
  await expect(page.getByText('0 checked')).toBeVisible();

  // Checked again, noticed only after moving on: Back, then Undo check.
  await page.getByRole('button', { name: 'Words are right' }).click();
  await expect(page.getByText('Line 2 of 3')).toBeVisible();
  await page.getByRole('button', { name: 'Back' }).click();
  await page.getByRole('button', { name: 'Undo check' }).click();
  await expect(page.getByRole('textbox', { name: 'What was said' })).toBeVisible();
  await expect(page.getByText('0 checked')).toBeVisible();

  const text = 'I have already made a list of errands.';
  expect(posted).toEqual([
    { id: 1, text, checked: true },
    { undoCheck: { id: 1 } },
    { id: 1, text, checked: true },
    { undoCheck: { id: 1 } },
  ]);
});

test('a line said by someone else is filed with their name (Pixel 9)', async ({ page }) => {
  const posted = await mockApi(page);
  await page.goto('/check');
  await page.getByRole('combobox', { name: 'Day' }).click();
  await page.getByRole('option', { name: 'Today' }).click();
  await expect(page.getByText('Line 1 of 3')).toBeVisible();

  const field = page.getByRole('combobox', { name: 'Someone else' });
  await expect(field).toBeInViewport();
  await field.fill('Dr');
  await page.getByRole('option', { name: 'Dr. Lee' }).click();
  // A different speaker is a change: the words stand, the name is new.
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText('Line 2 of 3')).toBeVisible();

  expect(posted).toEqual([
    { id: 1, text: 'I have already made a list of errands.', checked: true, speaker: 'Dr. Lee' },
  ]);
});
