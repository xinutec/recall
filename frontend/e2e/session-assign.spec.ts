import { expect, test, type Page, type Route } from '@playwright/test';

// Hermetic: every /api call is mocked — no real data, no backend.

function turn(id: number, speaker: string, cluster: string, text: string): unknown {
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
    audioUrl: `/api/audio/${id}`,
    source: 'm',
    cluster,
  };
}

const turns = [
  turn(1, 'Pippijn', 'SPEAKER_01', 'I have already made'),
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
      speakers: ['Pippijn', 'Dr. Lee'],
      preview: 'x',
      moments: [
        {
          start: '2026-01-15T09:35:50Z',
          end: '2026-01-15T09:35:55Z',
          primary: turns,
          alternates: [],
          sources: ['m'],
        },
      ],
    },
  ],
  hasMore: false,
};

// Mocks the API and returns a getter for the captured assign-span POST body.
async function mockApi(page: Page): Promise<() => unknown> {
  const captured: { body: unknown } = { body: null };
  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) return route.fulfill({ json: conversationPage });
    if (url.includes('/api/speakers')) {
      return route.fulfill({ json: { names: ['Pippijn', 'Dr. Lee'] } });
    }
    if (url.includes('/voices')) return route.fulfill({ json: { suggestions: {} } });
    return route.fulfill({ json: {} });
  });
  // Registered after the catch-all, so it wins for the assign POST.
  await page.route('**/api/sessions/*/assign', async (route: Route) => {
    captured.body = route.request().postDataJSON();
    await route.fulfill({ json: { touched: 1 } });
  });
  return () => captured.body;
}

// Selects "a list of errands" at the start of Dr. Lee's turn and fires the handler.
async function selectPhrase(page: Page): Promise<void> {
  await page.locator('span.t[data-id="2"]').waitFor();
  await page.evaluate(() => {
    const span = document.querySelector('span.t[data-id="2"]')!;
    const node = span.firstChild!;
    const range = document.createRange();
    range.setStart(node, 0);
    range.setEnd(node, 'a list of errands'.length);
    const sel = window.getSelection()!;
    sel.removeAllRanges();
    sel.addRange(range);
    document
      .querySelector('.transcript')!
      .dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  });
}

test('drag-select a phrase and assign it to an existing speaker (Pixel 9)', async ({ page }) => {
  const assign = await mockApi(page);
  await page.goto('/sessions/test');
  await selectPhrase(page);

  const bar = page.locator('.palette[role="toolbar"]');
  await expect(bar).toContainText('a list of errands');
  await bar.getByRole('button', { name: 'Pippijn' }).click();

  expect(assign()).toEqual({
    startTurn: 2,
    startChar: 0,
    endTurn: 2,
    endChar: 17,
    name: 'Pippijn',
  });
});

test('assign a phrase to a brand-new speaker via the name field (Pixel 9)', async ({ page }) => {
  const assign = await mockApi(page);
  await page.goto('/sessions/test');
  await selectPhrase(page);

  // The session only knows Pippijn / Dr. Lee; introduce a third person by name.
  const input = page.locator('.palette[role="toolbar"] input.new-name');
  await input.fill('Sam');
  await input.press('Enter');

  expect(assign()).toEqual({
    startTurn: 2,
    startChar: 0,
    endTurn: 2,
    endChar: 17,
    name: 'Sam',
  });
});
