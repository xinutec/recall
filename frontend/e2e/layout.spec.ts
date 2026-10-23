import { test, type Route } from '@playwright/test';
// Checks that only a painted layout shows: overlapping text, horizontal
// overflow, controls behind the bottom nav, clipped icons.
import {
  expectViewportIsPhone,
  expectIconFontLoaded,
  expectCleanLayout,
  expectNoOccludedControls,
  expectRecoversFromMissingBundle,
} from '@xinutec/ui-harness';
import { routesFrom, screens } from '@xinutec/ui-harness/routes';

import type {
  CaptureState,
  ConversationPage,
  LabelList,
  SessionList,
  SpeakerNames,
  Tier,
  Transcript,
  TranscriptList,
  VocabularyList,
} from '../src/app/models';

// Every /api call is mocked, with enough content (several speakers, long
// turns) to stress a phone column.
function turn(
  id: number,
  speaker: string,
  cluster: string,
  text: string,
  tier: Tier = 'diarized',
): Transcript {
  return {
    id,
    start: '2026-01-15T09:35:50Z',
    end: '2026-01-15T09:35:55Z',
    text,
    language: 'en',
    speaker: { kind: 'named', name: speaker },
    confidence: 0.9,
    model: tier,
    tier,
    hidden: null,
    hiddenAs: null,
    audioUrl: `/api/audio/${id}`,
    source: 'm',
    cluster,
    wordsChecked: false,
  };
}

// Invented names only (scripts/check-pii.sh).
const turns = [
  turn(1, 'Oskar', 'SPEAKER_01', 'I have already made a list of errands for the afternoon.'),
  turn(2, 'Oskar', 'SPEAKER_01', 'The first one is picking up a parcel from the depot.'),
  turn(3, 'Alex', 'SPEAKER_02', 'Let us go through them one by one so nothing is missed.'),
];

// Live turns only: the "Still being finalized" banner, whose longer sentence is
// what clipped its icon.
const provisionalTurns = [
  turn(1, 'Oskar', 'SPEAKER_01', 'I have already made a list of errands.', 'live'),
];

function pageOf(items: Transcript[]): ConversationPage {
  return {
    items: [
      {
        start: '2026-01-15T09:35:50Z',
        end: '2026-01-15T09:36:10Z',
        turnCount: items.length,
        speakers: ['Oskar', 'Alex'],
        preview: 'x',
        moments: items.map((t) => ({
          start: '2026-01-15T09:35:50Z',
          end: '2026-01-15T09:36:10Z',
          primary: t,
          alternates: [],
          sources: ['m'],
        })),
      },
    ],
    hasMore: false,
  };
}

const conversationPage = {
  items: pageOf(turns).items,
  hasMore: false,
} satisfies ConversationPage;

test.beforeEach(async ({ page }) => {
  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) return route.fulfill({ json: conversationPage });
    if (url.includes('/api/speakers'))
      return route.fulfill({ json: { names: ['Oskar', 'Alex'] } satisfies SpeakerNames });
    return route.fulfill({ status: 204, body: '' });
  });
});

test('session screen holds phone geometry with no overflow, overlap, or occlusion', async ({
  page,
}, testInfo) => {
  await page.goto('/sessions/test');
  await page.locator('.run .play').first().waitFor(); // the transcript has rendered

  await expectViewportIsPhone(page); // the checker-checker: really at phone width
  await expectIconFontLoaded(page); // Material Icons bundled, not tofu boxes
  await expectCleanLayout(page, testInfo);
  // `.t` spans are exempt: a wrapped inline span's centre lands on its parent
  // paragraph, which the probe reads as occluding it.
  await expectNoOccludedControls(page, testInfo, 'button, a[href], [role="button"]', ['.t']);
});

test('finalizing banner keeps its icon whole', async ({ page }, testInfo) => {
  // Registered after the beforeEach handler, so Playwright prefers it.
  await page.route('**/api/conversations**', (route: Route) =>
    route.fulfill({ json: pageOf(provisionalTurns) }),
  );
  await page.goto('/sessions/test');
  await page.locator('.status.finalizing').waitFor();

  await expectCleanLayout(page, testInfo);
});

// The recording banner in each state, and the menu that opens a window: the
// banner's sentence and buttons share a phone row.
const window = (overrides: Partial<CaptureState>): CaptureState => ({
  running: false,
  until: null,
  held: false,
  desiredRunning: false,
  desiredUntil: null,
  desiredHeld: false,
  settled: true,
  micReachable: true,
  stateToken: 'x',
  ...overrides,
});
const END = new Date(Date.now() + 3 * 3_600_000).toISOString();
const banners = {
  recording: window({ running: true, until: END, desiredRunning: true, desiredUntil: END }),
  held: window({ until: END, held: true, desiredUntil: END, desiredHeld: true }),
  fault: window({ running: true, desiredRunning: true }),
} satisfies Record<string, CaptureState>;

for (const [mode, state] of Object.entries(banners)) {
  test(`the ${mode} banner holds phone geometry`, async ({ page }, testInfo) => {
    await page.route('**/api/capture**', (route: Route) => route.fulfill({ json: state }));
    await page.goto('/sessions/test');
    await page.locator('.capture-banner .rec-dot').waitFor();
    await page.locator('.run .play').first().waitFor();

    await expectCleanLayout(page, testInfo);
    await expectNoOccludedControls(page, testInfo, '.capture-banner button');
  });
}

test('the recording menu holds phone geometry', async ({ page }, testInfo) => {
  await page.route('**/api/capture**', (route: Route) =>
    route.fulfill({ json: banners.recording }),
  );
  await page.goto('/sessions/test');
  await page.locator('.capture-toggle').click();
  await page.locator('.capture-menu .record-until input').waitFor();

  // The menu floats over the page: only its own content must not collide.
  await expectCleanLayout(page, testInfo, { root: '.capture-menu' });
  await expectNoOccludedControls(page, testInfo, '.capture-menu button, .capture-menu input');
});

// ---------------------------------------------------------------------------
// The same checks on every other routed screen (#1342).

const LONG =
  'a considerably longer stretch of household conversation that would overflow a phone column if the layout ever stopped wrapping it correctly';

const reviewItems = { items: [turn(21, 'Oskar', 'SPEAKER_01', LONG)] } satisfies TranscriptList;

const searchItems = { items: [turn(31, 'Alex', 'SPEAKER_02', LONG)] } satisfies TranscriptList;

const correctionsList = {
  items: [
    {
      id: 1,
      text: LONG,
      speaker: 'Oskar',
      language: 'en',
      start: '2026-01-15T09:35:50Z',
      audioUrl: '/api/correction/1/audio',
    },
  ],
  bySpeaker: { Oskar: 1 },
} satisfies LabelList;

const sessionsList = {
  items: [
    {
      id: 'meeting-20260115-0935',
      title: 'A meeting whose title is long enough to need wrapping on a phone list row',
      start: '2026-01-15T09:35:50Z',
      end: '2026-01-15T10:36:10Z',
      turnCount: 42,
      speakers: ['Oskar', 'Alex'],
      language: 'nl',
    },
  ],
} satisfies SessionList;

const screenMocks: Record<string, TranscriptList | LabelList | SessionList | VocabularyList | CaptureState> = {
  '/api/transcripts': reviewItems,
  '/api/search': searchItems,
  '/api/corrections': correctionsList,
  '/api/sessions': sessionsList,
  '/api/vocabulary': { items: [{ id: 1, term: 'vorasidenib' }] } satisfies VocabularyList,
  '/api/capture': {
    running: false,
    until: null,
    held: false,
    desiredRunning: false,
    desiredUntil: null,
    desiredHeld: false,
    settled: true,
    micReachable: true,
    stateToken: 'x',
  } satisfies CaptureState,
};

/**
 * What each routed screen draws once loaded, and any query it needs. The routes
 * come from `routes.json`, the route table; one added there without an entry
 * here fails rather than going unchecked.
 */
const LOADED: Record<string, { query?: string; anchor: string }> = {
  '/': { anchor: '.turns' },
  '/search': { anchor: '.search-field' },
  '/check': { query: '?ids=21', anchor: '.line' },
  '/labels': { anchor: '.vocab' },
  '/sessions': { anchor: '.page' },
  '/sessions/:id': { anchor: '.run .play' },
};

for (const { route, url } of screens(routesFrom('e2e/routes.json'), { id: 'test' })) {
  const loaded = LOADED[route];
  const path = url + (loaded?.query ?? '');
  test(`${path} holds phone geometry`, async ({ page }, testInfo) => {
    if (!loaded) throw new Error(`${route} is routed but has no entry in LOADED`);
    const { anchor } = loaded;
    await page.route('**/api/**', (route: Route) => {
      const url = new URL(route.request().url());
      for (const [prefix, json] of Object.entries(screenMocks)) {
        if (url.pathname === prefix || url.pathname.startsWith(prefix + '?')) {
          return route.fulfill({ json });
        }
      }
      if (url.pathname.includes('/api/conversations')) {
        return route.fulfill({ json: conversationPage });
      }
      return route.fulfill({ status: 204, body: '' });
    });
    await page.goto(path);
    await page.locator(anchor).first().waitFor();

    await expectViewportIsPhone(page);
    await expectIconFontLoaded(page);
    await expectCleanLayout(page, testInfo);
    // A disabled button has pointer-events none, so the probe would read it as
    // occluded by its ancestor.
    await expectNoOccludedControls(
      page,
      testInfo,
      'button:not([disabled]), a[href], [role="button"]',
      ['.t'],
    );
  });
}

// A service worker can serve an index naming a bundle a later deploy removed,
// and the update handling is inside that bundle (#1825). The recovery is inline
// in `src/index.html`.
test('a bundle a deploy removed reloads into the app, not a blank screen', async ({ page }) => {
  await expectRecoversFromMissingBundle(page, '/', 'h2:text-is("Timeline")');
});
