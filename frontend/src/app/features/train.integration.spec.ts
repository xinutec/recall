import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { NavigationEnd, provideRouter, Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { MatSnackBar } from '@angular/material/snack-bar';
import { filter, firstValueFrom } from 'rxjs';
import { vi } from 'vitest';

import { Train } from './train';

/**
 * Integration test through the REAL router + real HTTP layer — no mocked
 * navigate(). This is the chain that broke in the field: a windowed URL must
 * refetch /api/train *with the window*, and pressing Apply must get you there.
 * The unit tests mock the router, so they can't see this seam; this one can.
 *
 * Note: recall-api builds the query string into the URL itself, so the window
 * shows up in `req.url` (".../api/train?...since="), not in HttpParams.
 */
describe('Train (router integration)', () => {
  async function start(url = '/train') {
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([{ path: 'train', component: Train }]),
        provideHttpClient(),
        provideHttpClientTesting(),
        { provide: MatSnackBar, useValue: { open: vi.fn() } },
      ],
    });
    const harness = await RouterTestingHarness.create();
    const c = (await harness.navigateByUrl(url, Train)) as unknown as {
      from: { set(v: string): void };
      applyRange(): void;
    };
    const http = TestBed.inject(HttpTestingController);
    // The constructor fetches the quick-pick roster; drain it so the per-test
    // verify() only sees the /api/train requests under test.
    http.expectOne('/api/speakers').flush({ names: ['Alice', 'Bob', 'Carol', 'Pippijn'] });
    return { harness, c, http };
  }

  const trainReq = (http: HttpTestingController) =>
    http.expectOne((r) => r.url.startsWith('/api/train'));

  it('landing on a windowed URL fetches the queue scoped to the window', async () => {
    const { http } = await start('/train?from=2026-06-14T18:00&to=2026-06-14T19:00');
    const req = trainReq(http);
    expect(req.request.url).toContain('since=');
    expect(req.request.url).toContain('until=');
    req.flush({ items: [], corrections: 0 });
    http.verify();
  });

  it('Apply moves to the windowed URL and refetches with the window', async () => {
    const { harness, c, http } = await start();
    const router = TestBed.inject(Router);

    // Initial load on entering /train: no window yet → bare request.
    const initial = trainReq(http);
    expect(initial.request.url).not.toContain('since=');
    initial.flush({ items: [], corrections: 0 });

    // User picks a start time and presses Apply; await the navigation settling.
    const navigated = firstValueFrom(router.events.pipe(filter((e) => e instanceof NavigationEnd)));
    c.from.set('2026-06-14T18:00');
    c.applyRange();
    await navigated;
    await harness.fixture.whenStable();

    expect(router.url).toContain('from=');
    const windowed = trainReq(http);
    expect(windowed.request.url).toContain('since=');
    windowed.flush({ items: [], corrections: 0 });
    http.verify();
  });
});
