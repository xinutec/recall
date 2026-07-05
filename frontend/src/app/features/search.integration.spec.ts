import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideRouter, withComponentInputBinding } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';

import { Search } from './search';

/**
 * Integration test through the REAL router with withComponentInputBinding(), as
 * in app.config.ts. It proves what the unit spec can't: when /search is opened
 * without a ?q param, the router calls setInput('q', undefined) — clobbering the
 * input('') default. Untransformed, that undefined reached the DOM as the literal
 * string "undefined" and made settledQuery().trim() throw ("Search failed").
 */
describe('Search (router integration)', () => {
  async function start(url: string) {
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([{ path: 'search', component: Search }], withComponentInputBinding()),
        provideHttpClient(),
        provideHttpClientTesting(),
      ],
    });
    const harness = await RouterTestingHarness.create();
    await harness.navigateByUrl(url, Search);
    const http = TestBed.inject(HttpTestingController);
    return { harness, http };
  }

  const settle = () => new Promise((resolve) => setTimeout(resolve, 250)); // past debounce

  it('opens without ?q: empty box, no error, no request', async () => {
    const { harness, http } = await start('/search');
    await settle();
    harness.detectChanges();
    const el = harness.routeNativeElement!;
    const box = el.querySelector<HTMLInputElement>('input[type=search]');
    expect(box?.value).toBe('');
    expect(el.textContent).not.toContain('Search failed');
    http.expectNone((r) => r.url.startsWith('/api/search'));
    http.verify();
  });

  it('opens with ?q=iets: box pre-filled, one FTS request fires', async () => {
    const { harness, http } = await start('/search?q=iets');
    await settle();
    harness.detectChanges();
    const el = harness.routeNativeElement!;
    const box = el.querySelector<HTMLInputElement>('input[type=search]');
    expect(box?.value).toBe('iets');
    const req = http.expectOne((r) => r.url.startsWith('/api/search') && r.url.includes('q=iets'));
    req.flush({ items: [] });
    http.verify();
  });
});
