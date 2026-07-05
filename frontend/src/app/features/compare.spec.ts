import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, Router } from '@angular/router';
import { describe, expect, it, vi } from 'vitest';

import { Compare } from './compare';

function setup() {
  const navigate = vi.fn();
  TestBed.configureTestingModule({
    providers: [
      provideZonelessChangeDetection(),
      provideHttpClient(),
      provideHttpClientTesting(),
      { provide: Router, useValue: { navigate } },
      { provide: ActivatedRoute, useValue: {} },
    ],
  });
  const fixture = TestBed.createComponent(Compare);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const c = fixture.componentInstance as any;
  const http = TestBed.inject(HttpTestingController);
  return { c, navigate, http };
}

describe('Compare', () => {
  it('does nothing without a source', () => {
    const { c } = setup();
    expect(c.canStart()).toBe(false);
    c.start();
    // no request asserted — start() returns early
  });

  it('posts only the filled fields and opens the new run', () => {
    const { c, navigate, http } = setup();
    c.source.set('meeting-1');
    c.modelB.set('/x/adapter');
    expect(c.canStart()).toBe(true);

    c.start();
    const req = http.expectOne('/api/ab-compare');
    expect(req.request.body).toEqual({ source: 'meeting-1', modelB: '/x/adapter' });
    req.flush({ newId: 12 });

    expect(navigate).toHaveBeenCalledWith(['/compare', 12]);
  });

  it('surfaces an error and re-enables the button on failure', () => {
    const { c, http } = setup();
    c.source.set('usb');
    c.start();
    http.expectOne('/api/ab-compare').flush('nope', { status: 500, statusText: 'Server Error' });
    expect(c.error()).not.toBe('');
    expect(c.starting()).toBe(false);
  });
});

describe('Compare — polling lifecycle', () => {
  it('clears its status poller when the component is destroyed', () => {
    // A destroyed Compare page must not keep its 5s poller alive for the tab's
    // lifetime, retaining the component and hitting the API from a dead page.
    vi.useFakeTimers();
    try {
      const navigate = vi.fn();
      TestBed.configureTestingModule({
        providers: [
          provideZonelessChangeDetection(),
          provideHttpClient(),
          provideHttpClientTesting(),
          { provide: Router, useValue: { navigate } },
          { provide: ActivatedRoute, useValue: {} },
        ],
      });
      const fixture = TestBed.createComponent(Compare);
      const withPoller = vi.getTimerCount();
      fixture.destroy();
      expect(vi.getTimerCount()).toBe(withPoller - 1);
    } finally {
      vi.useRealTimers();
    }
  });
});
