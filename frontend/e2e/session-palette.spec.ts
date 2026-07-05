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

test('Fix words is reachable, not hidden behind the bottom nav (Pixel 9)', async ({ page }) => {
  await mockApi(page, SHORT);
  await page.goto('/sessions/test');

  const line = page.locator('span.t', { hasText: 'a list of errands' });
  await expect(line).toBeVisible();

  await line.click();
  const fix = page.getByRole('button', { name: /fix words/i });
  await expect(fix).toBeVisible();

  await fix.scrollIntoViewIfNeeded();
  await expect(fix).toBeInViewport();
  const fixBox = await fix.boundingBox();
  const navBox = await page.locator('nav.bottom-nav').boundingBox();
  expect(fixBox).not.toBeNull();
  expect(navBox).not.toBeNull();
  expect(fixBox!.y + fixBox!.height).toBeLessThanOrEqual(navBox!.y);
});

test('Fix words stays on screen after selecting a line in a long transcript', async ({ page }) => {
  await mockApi(page, LONG);
  await page.goto('/sessions/test');

  // Tap a line near the TOP of a long transcript. The selection toolbar must come to
  // the user — visible in the viewport without hunting to the bottom of the page.
  const line = page.locator('span.t', { hasText: 'My very first question' });
  await expect(line).toBeVisible();
  await line.click();

  const fix = page.getByRole('button', { name: /fix words/i });
  await expect(fix).toBeVisible();
  // No scrollIntoView: it should already be on screen, above the bottom nav.
  await expect(fix).toBeInViewport();
  const fixBox = await fix.boundingBox();
  const navBox = await page.locator('nav.bottom-nav').boundingBox();
  expect(fixBox).not.toBeNull();
  expect(navBox).not.toBeNull();
  expect(fixBox!.y + fixBox!.height).toBeLessThanOrEqual(navBox!.y);
});
