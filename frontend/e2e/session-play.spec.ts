import { expect, test, type Route } from '@playwright/test';

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

// Two same-speaker turns coalesce into ONE bubble (run) — the case where the play
// button must span both turns, not just the first.
const turns = [
  turn(1, 'Pippijn', 'SPEAKER_01', 'I have already made a list of errands.'),
  turn(2, 'Pippijn', 'SPEAKER_01', 'The first one is about the molecular pathology.'),
];

const conversationPage = {
  items: [
    {
      start: '2026-01-15T09:35:50Z',
      end: '2026-01-15T09:36:10Z',
      turnCount: 2,
      speakers: ['Pippijn'],
      preview: 'x',
      moments: [
        {
          start: '2026-01-15T09:35:50Z',
          end: '2026-01-15T09:36:10Z',
          primary: turns,
          alternates: [],
          sources: ['m'],
        },
      ],
    },
  ],
  hasMore: false,
};

test('the bubble play button requests the full joined span (Pixel 9)', async ({ page }) => {
  let spanUrl: string | null = null;

  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) return route.fulfill({ json: conversationPage });
    if (url.includes('/api/speakers')) return route.fulfill({ json: { names: ['Pippijn'] } });
    if (url.includes('/voices')) return route.fulfill({ json: { suggestions: {} } });
    return route.fulfill({ json: {} });
  });
  // Registered after the catch-all, so it wins for the span request.
  await page.route('**/api/audio-span*', async (route: Route) => {
    spanUrl = route.request().url();
    await route.fulfill({ status: 200, contentType: 'audio/wav', body: Buffer.from('') });
  });

  await page.goto('/sessions/test');
  const play = page.locator('.run .play').first();
  await play.waitFor();
  await play.click();

  // The whole bubble plays as one span across both turns, not just the first.
  await expect.poll(() => spanUrl).toContain('/api/audio-span?from_id=1&to_id=2');
});
