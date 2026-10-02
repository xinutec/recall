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
  expectRecoversFromMissingBundle,
} from '@xinutec/ui-harness';

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

// Hermetic: every /api call is mocked — no real data, no backend. A rich session
// (multiple speakers, a long turn that would overflow a phone column) so the layout
// checks have real content to measure.
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
    speaker,
    speakerConfirmed: true,
    speakerConfidence: null,
    confidence: 0.9,
    loudness: 0.01,
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

// Synthetic speakers/content only — no real names (see scripts/check-pii.sh).
const turns = [
  turn(1, 'Oskar', 'SPEAKER_01', 'I have already made a list of errands for the afternoon.'),
  turn(2, 'Oskar', 'SPEAKER_01', 'The first one is picking up a parcel from the depot.'),
  turn(3, 'Alex', 'SPEAKER_02', 'Let us go through them one by one so nothing is missed.'),
];

// The same session before the refine pass lands: provisional turns put the screen in
// its "Still being finalized" state, whose banner carries a much longer sentence than
// the finalized one. That length is the variable that clips the icon, so the state has
// to be rendered to be checked.
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
    running: true,
    desiredRunning: true,
    settled: true,
    micReachable: true,
    pausedUntil: null,
    desiredPausedUntil: null,
    stateToken: 'x',
  } satisfies CaptureState,
};

const screens: { path: string; anchor: string }[] = [
  { path: '/', anchor: '.turns' },
  { path: '/search', anchor: '.search-field' },
  { path: '/check?ids=21', anchor: '.line' },
  { path: '/labels', anchor: '.vocab' },
  { path: '/sessions', anchor: '.page' },
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
      return route.fulfill({ status: 204, body: '' });
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
    await expectNoOccludedControls(
      page,
      testInfo,
      'button:not([disabled]), a[href], [role="button"]',
      ['.t'],
    );
    await expectNoClippedIcons(page, testInfo);
  });
}

// A service worker can serve an index naming a bundle a later deploy removed, and
// the app's own update handling is inside that bundle (#1825). The recovery is
// inline in `src/index.html`; this is the check that it is there and works.
test('a bundle a deploy removed reloads into the app, not a blank screen', async ({ page }) => {
  await expectRecoversFromMissingBundle(page, '/', 'h2:text-is("Timeline")');
});
