import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { NavigationEnd, provideRouter, Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { filter, firstValueFrom } from 'rxjs';

import { Timeline } from './timeline';

/**
 * Through the real router: the timeline route requests /api/conversations, and
 * a `?before=` cursor reaches it. The unit spec covers the rest.
 *
 * Never `whenStable`: a pending httpResource request keeps the fixture
 * unstable, so the requests are drained instead.
 */
describe('Timeline (router integration)', () => {
  async function start(url = '/timeline') {
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([{ path: 'timeline', component: Timeline }]),
        provideHttpClient(),
        provideHttpClientTesting(),
      ],
    });
    const harness = await RouterTestingHarness.create();
    await harness.navigateByUrl(url, Timeline);
    const http = TestBed.inject(HttpTestingController);
    return { harness, http };
  }

  const drain = (http: HttpTestingController) =>
    http
      .match((r) => r.url.startsWith('/api/conversations'))
      .forEach((r) => r.flush({ items: [], hasMore: false }));

  it('lands on /timeline and fetches the latest window from /api/conversations', async () => {
    const { http } = await start('/timeline');
    const reqs = http.match((r) => r.url.startsWith('/api/conversations'));
    expect(reqs.length).toBeGreaterThan(0);
    expect(reqs.at(-1)?.request.url).toContain('limit=');
    expect(reqs.at(-1)?.request.url).not.toContain('before=');
    drain(http);
  });

  it('a ?before cursor round-trips through the real router onto the timeline route', async () => {
    const { harness, http } = await start('/timeline');
    const router = TestBed.inject(Router);
    drain(http);

    const navigated = firstValueFrom(router.events.pipe(filter((e) => e instanceof NavigationEnd)));
    await harness.navigateByUrl('/timeline?before=2026-06-13T09:00:00Z', Timeline);
    await navigated;
    expect(router.url).toContain('before=2026-06-13T09:00:00Z');
    drain(http);
  });
});
