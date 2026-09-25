import { expect, test, type Page, type Route } from '@playwright/test';

// Hermetic: every /api call is mocked here, so the e2e touches no real data.

interface FakeTurn {
  id: number;
  start: string;
  end: string;
  text: string;
  language: string;
  speaker: string;
  speakerConfirmed: boolean;
  speakerConfidence: number | null;
  confidence: number | null;
  loudness: number | null;
  model: string;
  tier: string;
  hidden: string | null;
  audioUrl: string;
  source: string;
  cluster: string;
}

function turn(id: number, speaker: string, cluster: string, text: string): FakeTurn {
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
    tier: 'diarized', // diarized => session is "ready", so the palette is offered
    hidden: null,
    audioUrl: `/api/audio/${id}`,
    source: 'm',
    cluster,
  };
}

function makePage(turns: FakeTurn[]): unknown {
  return {
    items: [
      {
        start: turns[0].start,
        end: turns[turns.length - 1].end,
        turnCount: turns.length,
        speakers: ['Pippijn', 'Dr. Lee'],
        preview: 'x',
        moments: [
          {
            start: turns[0].start,
            end: turns[turns.length - 1].end,
            primary: turns,
            alternates: [],
            sources: ['m'],
          },
        ],
      },
    ],
    hasMore: false,
  };
}

async function mockApi(page: Page, turns: FakeTurn[]): Promise<void> {
  const conversationPage = makePage(turns);
  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) return route.fulfill({ json: conversationPage });
    if (url.includes('/api/speakers')) {
      return route.fulfill({ json: { names: ['Pippijn', 'Dr. Lee'] } });
    }
    if (url.includes('/voices')) return route.fulfill({ json: { suggestions: {} } });
    return route.fulfill({ json: {} });
  });
}

const SHORT = [
  turn(1, 'Pippijn', 'SPEAKER_01', 'I have already made'),
  turn(
    2,
    'Dr. Lee',
    'SPEAKER_00',
    'a list of errands and we want to make sure we get through every one of them today',
  ),
];

// Long enough that the transcript scrolls well past one screen — the case where an
// in-flow toolbar would land far below the tapped line.
const LONG = Array.from({ length: 40 }, (_, i) => {
  if (i === 0) return turn(1, 'Pippijn', 'SPEAKER_01', 'My very first question for you today.');
  return i % 2 === 0
    ? turn(i + 1, 'Pippijn', 'SPEAKER_01', `Point number ${i + 1} that I wanted to raise today.`)
    : turn(i + 1, 'Dr. Lee', 'SPEAKER_00', `Right, and the answer to ${i + 1} is as follows.`);
});

async function tapFixWords(page: Page, text: string): Promise<void> {
  const line = page.locator('span.t', { hasText: text });
  await expect(line).toBeVisible();
  await line.click();
  const fix = page.getByRole('button', { name: /fix words/i });
  // No scrolling: the sheet comes to the thumb, wherever the line is.
  await expect(fix).toBeInViewport();
  await fix.click();
  await expect(page.getByLabel('Words')).toBeInViewport();
  await expect(page.getByRole('button', { name: 'Save' })).toBeInViewport();
}

test('Fix words opens in the sheet, on screen (Pixel 9)', async ({ page }) => {
  await mockApi(page, SHORT);
  await page.goto('/sessions/test');
  await tapFixWords(page, 'a list of errands');
});

test('Fix words is on screen for a line at the top of a long transcript', async ({ page }) => {
  await mockApi(page, LONG);
  await page.goto('/sessions/test');
  await tapFixWords(page, 'My very first question');
});

test('naming a voice from a suggestion saves only that name (Pixel 9)', async ({ page }) => {
  await mockApi(page, SHORT);
  const posted: unknown[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().endsWith('/voice')) posted.push(r.postDataJSON());
  });
  await page.goto('/sessions/test');
  const field = page.getByRole('combobox', { name: 'Name for Voice 1' });
  await field.fill('Dr');
  await page.getByRole('option', { name: 'Dr. Lee' }).click();
  await expect.poll(() => posted).toEqual([{ cluster: 'SPEAKER_01', name: 'Dr. Lee' }]);
  // Tapping away afterwards changes nothing.
  await page.locator('h3', { hasText: "Who's speaking" }).click();
  await page.waitForTimeout(200);
  expect(posted).toHaveLength(1);
});

test('a typed name is saved on tapping away, with or without suggestions open', async ({ page }) => {
  await mockApi(page, SHORT);
  const posted: unknown[] = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url().endsWith('/voice')) posted.push(r.postDataJSON());
  });
  await page.goto('/sessions/test');
  const heading = page.locator('h3', { hasText: "Who's speaking" });
  await page.getByRole('combobox', { name: 'Name for Voice 1' }).fill('Sam');
  await heading.click();
  await page.getByRole('combobox', { name: 'Name for Voice 2' }).fill('Pip');
  await expect(page.getByRole('option', { name: 'Pippijn' })).toBeVisible();
  await heading.click();
  await expect
    .poll(() => posted)
    .toEqual([
      { cluster: 'SPEAKER_01', name: 'Sam' },
      { cluster: 'SPEAKER_00', name: 'Pip' },
    ]);
});
