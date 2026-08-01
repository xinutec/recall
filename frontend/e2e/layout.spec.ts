import { test, type Route } from '@playwright/test';
// The fleet-shared layout harness, consumed as the published @xinutec/ui-harness
// package (source repo ~/Code/ui-harness). It renders the app in a real browser at
// true phone geometry and asserts the failure classes that read fine in source and
// only show in a painted layout — text collisions, horizontal overflow, and
// controls occluded behind the fixed bottom nav.
import {
  expectViewportIsPhone,
  expectIconFontLoaded,
  expectNoHorizontalOverflow,
  expectNoTextOverlaps,
  expectNoOccludedControls,
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

// An icon painted narrower than its glyph. `mat-icon` carries `overflow: hidden`,
// which voids the `min-width: auto` floor that stops a flex item collapsing below
// its content — so beside a long text sibling the icon is silently CLIPPED rather
// than scaled, and shows as a fragment. None of the harness checks see this: it
// causes no overflow (shrinking is what avoids it), no overlap, no occlusion, and
// findClippedText deliberately exempts icon ligatures and only looks at vertical
// shear. Local until @xinutec/ui-harness grows an icon-geometry check.
async function expectIconsNotClipped(page: import('@playwright/test').Page): Promise<void> {
  const clipped = await page.evaluate(() =>
    Array.from(document.querySelectorAll('mat-icon'))
      .filter((el) => {
        const r = el.getBoundingClientRect();
        return r.width > 0 && el.scrollWidth - el.clientWidth > 1;
      })
      .map((el) => ({
        icon: el.textContent?.trim() ?? '',
        painted: Math.round(el.getBoundingClientRect().width * 100) / 100,
        glyph: el.scrollWidth,
      })),
  );
  if (clipped.length > 0) {
    const detail = clipped
      .map((c) => `  ${c.icon}: ${c.painted}px of a ${c.glyph}px glyph`)
      .join('\n');
    throw new Error(`Icons clipped by their own overflow box (${clipped.length}):\n${detail}`);
  }
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
  await expectIconsNotClipped(page);
});

test('finalizing banner keeps its icon whole', async ({ page }, testInfo) => {
  // Registered after the beforeEach handler, so Playwright prefers it.
  await page.route('**/api/conversations**', (route: Route) =>
    route.fulfill({ json: pageOf(provisionalTurns) }),
  );
  await page.goto('/sessions/test');
  await page.locator('.status.finalizing').waitFor();

  await expectIconsNotClipped(page);
  await expectNoHorizontalOverflow(page, testInfo);
});
