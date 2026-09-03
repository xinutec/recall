import { test, type Route } from '@playwright/test';
// The fleet-shared layout harness, consumed as the published @xinutec/ui-harness
// package (source repo ~/Code/ui-harness). It renders the app in a real browser at
// true phone geometry and asserts the failure classes that read fine in source and
// only show in a painted layout — text collisions, horizontal overflow, controls
// occluded behind the fixed bottom nav, and icons squeezed below their own glyph.
import {
  expectViewportIsPhone,
  expectIconFontLoaded,
  expectNoHorizontalOverflow,
  expectNoTextOverlaps,
  expectNoOccludedControls,
  expectNoClippedIcons,
} from '@xinutec/ui-harness';

// Hermetic: every /api call is mocked — no real data, no backend. A rich session
// (multiple speakers, a long turn that would overflow a phone column) so the layout
// checks have real content to measure.
function turn(
  id: number,
  speaker: string,
  cluster: string,
  text: string,
  tier = 'diarized',
): unknown {
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
    model: tier,
    tier,
    hidden: null,
    audioUrl: `/api/audio/${id}`,
    source: 'm',
    cluster,
  };
}

// Synthetic speakers/content only — no real names (see scripts/check-pii.sh).
const turns = [
  turn(1, 'Pippijn', 'SPEAKER_01', 'I have already made a list of errands for the afternoon.'),
  turn(2, 'Pippijn', 'SPEAKER_01', 'The first one is picking up a parcel from the depot.'),
  turn(3, 'Alex', 'SPEAKER_02', 'Let us go through them one by one so nothing is missed.'),
];

// The same session before the refine pass lands: provisional turns put the screen in
// its "Still being finalized" state, whose banner carries a much longer sentence than
// the finalized one. That length is the variable that clips the icon, so the state has
// to be rendered to be checked.
const provisionalTurns = [
  turn(1, 'Pippijn', 'SPEAKER_01', 'I have already made a list of errands.', 'live'),
];

function pageOf(items: unknown[]) {
  return {
    items: [
      {
        start: '2026-01-15T09:35:50Z',
        end: '2026-01-15T09:36:10Z',
        turnCount: items.length,
        speakers: ['Pippijn', 'Alex'],
        preview: 'x',
        moments: [
          {
            start: '2026-01-15T09:35:50Z',
            end: '2026-01-15T09:36:10Z',
            primary: items,
            alternates: [],
            sources: ['m'],
          },
        ],
      },
    ],
    hasMore: false,
  };
}

const conversationPage = {
  items: pageOf(turns).items,
  hasMore: false,
};

test.beforeEach(async ({ page }) => {
  await page.route('**/api/**', (route: Route) => {
    const url = route.request().url();
    if (url.includes('/api/conversations')) return route.fulfill({ json: conversationPage });
    if (url.includes('/api/speakers')) return route.fulfill({ json: { names: ['Pippijn', 'Alex'] } });
    if (url.includes('/voices')) return route.fulfill({ json: { suggestions: {} } });
    return route.fulfill({ json: {} });
  });
});

test('session screen holds phone geometry with no overflow, overlap, or occlusion', async ({
  page,
}, testInfo) => {
  await page.goto('/sessions/test');
  await page.locator('.run .play').first().waitFor(); // the transcript has rendered

  await expectViewportIsPhone(page); // the checker-checker: really at phone width
  await expectIconFontLoaded(page); // Material Icons bundled, not tofu boxes
  await expectNoHorizontalOverflow(page, testInfo);
  await expectNoTextOverlaps(page, testInfo);
  // The bottom nav is fixed — nothing tappable may hide behind it. Exempt the
  // transcript `.t` spans: they're inline click-to-select text (role=button), and
  // a wrapped inline span's bounding-box centre lands on its own <p class="body">
  // parent, which the centre-point occlusion model reads as occluded. The check
  // still guards the real block controls (nav, pause/resume, voice actions).
  await expectNoOccludedControls(page, testInfo, 'button, a[href], [role="button"]', ['.t']);
  await expectNoClippedIcons(page, testInfo);
});

test('finalizing banner keeps its icon whole', async ({ page }, testInfo) => {
  // Registered after the beforeEach handler, so Playwright prefers it.
  await page.route('**/api/conversations**', (route: Route) =>
    route.fulfill({ json: pageOf(provisionalTurns) }),
  );
  await page.goto('/sessions/test');
  await page.locator('.status.finalizing').waitFor();

  await expectNoClippedIcons(page, testInfo);
  await expectNoHorizontalOverflow(page, testInfo);
});

// ---------------------------------------------------------------------------
// Every routed screen, not just the session view (#1342's UI-quality pass).
// Nine screens had no painted evidence at phone geometry; the checks below are
// the same failure classes, per screen, over rich-enough mocked data that the
// phone column is actually stressed (a long turn, a long label, a long span).

const LONG = 'a considerably longer stretch of household conversation that would overflow a phone column if the layout ever stopped wrapping it correctly';

const trainItems = {
  items: [turn(11, 'Pippijn', 'SPEAKER_01', LONG), turn(12, 'Alex', 'SPEAKER_02', 'Short.')],
  corrections: 468,
};

const reviewItems = { items: [turn(21, 'Pippijn', 'SPEAKER_01', LONG)] };

const searchItems = { items: [turn(31, 'Alex', 'SPEAKER_02', LONG)] };

const correctionsList = {
  items: [
    {
      id: 1,
      speaker: 'Pippijn',
      original: 'a shorter original line',
      corrected: LONG,
      start: '2026-01-15T09:35:50Z',
      end: '2026-01-15T09:35:55Z',
      audioUrl: '/api/correction/1/audio',
      audioConfidence: 0.4,
      hidden: null,
    },
  ],
};

const sessionsList = {
  items: [
    {
      id: 'meeting-20260115-0935',
      title: 'A meeting whose title is long enough to need wrapping on a phone list row',
      start: '2026-01-15T09:35:50Z',
      end: '2026-01-15T10:36:10Z',
      turnCount: 42,
      speakers: ['Pippijn', 'Alex'],
    },
  ],
};

const quietSpans = {
  items: [
    {
      source: 'usb',
      start: '2026-01-15T02:00:00Z',
      end: '2026-01-15T04:10:00Z',
      durationS: 7800,
      audioIds: [1, 2, 3],
      soundSeconds: 4.2,
      loudestDb: -51.5,
      marginDb: 6.1,
      silent: false,
      structure: 0.4,
    },
  ],
};

const abRuns = {
  items: [
    {
      id: 1,
      source: 'meeting-20260115-0935',
      modelA: 'mlx-community/whisper-large-v3-turbo',
      modelB: 'adapter-current',
      baseModel: 'mlx-community/whisper-large-v3',
      status: 'done',
      created: '2026-01-15T09:00:00Z',
      meanWerA: 0.121,
      meanWerB: 0.098,
      nCorrections: 20,
      nSegments: 64,
      nChanged: 12,
      error: null,
    },
  ],
};

const summariesOut = {
  items: [
    { day: '2026-01-15', text: LONG, model: 'qwen' },
    { day: '2026-01-14', text: 'A quieter day.', model: 'qwen' },
  ],
};

const screenMocks: Record<string, unknown> = {
  '/api/train': trainItems,
  '/api/review': reviewItems,
  '/api/search': searchItems,
  '/api/corrections': correctionsList,
  '/api/sessions': sessionsList,
  '/api/quiet/spans': quietSpans,
  '/api/quiet/scan': { running: false, measured: 10, total: 10, analysed: 10, toAnalyse: 10 },
  '/api/ab-compare': abRuns,
  '/api/summaries': summariesOut,
  '/api/vocabulary': { items: [{ id: 1, term: 'vorasidenib' }] },
  '/api/context': { text: 'A household context paragraph.' },
  '/api/capture': { running: true, desiredRunning: true, settled: true, micReachable: true, pausedUntil: null, desiredPausedUntil: null, stateToken: 'x' },
  '/api/sources': { items: [{ id: 'usb', name: 'usb', kind: 'coreaudio', active: true, lastActive: '2026-01-15T09:35:50Z' }] },
};

const screens: { path: string; anchor: string }[] = [
  { path: '/', anchor: '.turns' },
  { path: '/search', anchor: '.search-field' },
  { path: '/ask', anchor: '.question' },
  { path: '/review', anchor: '.page' },
  { path: '/train', anchor: '.page' },
  { path: '/labels', anchor: '.vocab' },
  { path: '/cleanup', anchor: '.cleanup' },
  { path: '/sessions', anchor: '.page' },
  { path: '/compare', anchor: '.page' },
];

for (const { path, anchor } of screens) {
  test(`${path} holds phone geometry`, async ({ page }, testInfo) => {
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
      return route.fulfill({ json: {} });
    });
    await page.goto(path);
    await page.locator(anchor).first().waitFor();

    await expectViewportIsPhone(page);
    await expectIconFontLoaded(page);
    await expectNoHorizontalOverflow(page, testInfo);
    await expectNoTextOverlaps(page, testInfo);
    // :not([disabled]): a disabled Material button has pointer-events none, so
    // the centre-point probe reads its own ancestor and calls it occluded — but a
    // control that cannot be tapped anyway has no occlusion to answer for.
    await expectNoOccludedControls(page, testInfo, 'button:not([disabled]), a[href], [role="button"]', ['.t']);
    await expectNoClippedIcons(page, testInfo);
  });
}
