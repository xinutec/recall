import { expect, test, type Page, type Route } from '@playwright/test';

import type { AssignResult, ConversationPage, SpeakerNames, Transcript } from '../src/app/models';

// Hermetic: every /api call is mocked — no real data, no backend.

function turn(id: number, speaker: string, cluster: string, text: string): Transcript {
  return {
    id,
    start: '2026-01-15T09:35:50Z',
    end: '2026-01-15T09:35:55Z',
    text,
    language: 'en',
    speaker,
    speakerConfirmed: true,
    speakerConfidence: null,
    confidence: 0.9,
    loudness: 0.01,
    model: 'diarized',
    tier: 'diarized',
    hidden: null,
    hiddenAs: null,
    audioUrl: `/api/audio/${id}`,
    source: 'm',
    cluster,
    wordsChecked: false,
  };
}

const turns = [
  turn(1, 'Oskar', 'SPEAKER_01', 'I have already made'),
  turn(
    2,
    'Dr. Lee',
    'SPEAKER_00',
    'a list of errands and we want to make sure we get through every one of them today',
  ),
];

const conversationPage = {
  items: [
    {
      start: '2026-01-15T09:35:50Z',
      end: '2026-01-15T09:35:55Z',
      turnCount: 2,
      speakers: ['Oskar', 'Dr. Lee'],
      preview: 'x',
      moments: turns.map((t) => ({
        start: '2026-01-15T09:35:50Z',
        end: '2026-01-15T09:35:55Z',
        primary: t,
        alternates: [],
        sources: ['m'],
      })),
    },
  ],
  hasMore: false,
} satisfies ConversationPage;

// Mocks the API and returns a getter for the captured assign-span POST body.
async function mockApi(page: Page): Promise<() => unknown> {
  const captured: { body: unknown } = { body: null };
  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) return route.fulfill({ json: conversationPage });
    if (url.includes('/api/speakers')) {
      return route.fulfill({ json: { names: ['Oskar', 'Dr. Lee'] } satisfies SpeakerNames });
    }
    return route.fulfill({ status: 204, body: '' });
  });
  // Registered after the catch-all, so it wins for the assign POST.
  await page.route('**/api/sessions/*/assign', async (route: Route) => {
    captured.body = route.request().postDataJSON();
    await route.fulfill({ json: { touched: 1 } satisfies AssignResult });
  });
  return () => captured.body;
}

// Opens Dr. Lee's line and picks "a list of errands", its first four words.
async function pickPhrase(page: Page): Promise<void> {
  await page.locator('span.t', { hasText: 'a list of errands' }).click();
  await page.getByRole('button', { name: /part of it was someone else/i }).click();
  await page.getByRole('button', { name: 'a', exact: true }).click();
  await page.getByRole('button', { name: 'errands', exact: true }).click();
  await expect(page.getByText('“a list of errands” was said by')).toBeVisible();
}

const PART = { startTurn: 2, startChar: 0, endTurn: 2, endChar: 17 };

test('give part of a line to an existing speaker (Pixel 9)', async ({ page }) => {
  const assign = await mockApi(page);
  await page.goto('/sessions/test');
  await pickPhrase(page);
  await page.getByRole('option', { name: 'Oskar' }).click();
  await expect.poll(assign).toEqual({ ...PART, name: 'Oskar' });
});

test('give part of a line to a brand-new speaker (Pixel 9)', async ({ page }) => {
  const assign = await mockApi(page);
  await page.goto('/sessions/test');
  await pickPhrase(page);
  const field = page.getByRole('combobox', { name: 'Someone else' });
  await field.fill('Sam');
  await field.press('Enter');
  await expect.poll(assign).toEqual({ ...PART, name: 'Sam' });
});
