import { DOCUMENT, Injectable, inject } from '@angular/core';
import { NavigationEnd, Router } from '@angular/router';
import { filter } from 'rxjs';

import type { TelemetryEvent } from './models';

/**
 * The label of the nearest control at or above `node`: its aria-label, else its
 * visible text, else its title. Null outside a control, which keeps the trace
 * to deliberate taps. Exported for its test.
 */
export function labelFor(node: EventTarget | null): string | null {
  if (!(node instanceof Element)) return null;
  const el = node.closest(
    'button, a, [role="button"], [role="tab"], [role="menuitem"], [role="switch"], input[type="submit"]',
  );
  if (!el) return null;
  const aria = el.getAttribute('aria-label')?.trim();
  if (aria) return aria;

  // Without icons, whose ligature names are text ("micRecord"), and without
  // aria-hidden content. On a clone, so the page is untouched.
  const clone = el.cloneNode(true);
  let text = '';
  if (clone instanceof Element) {
    clone.querySelectorAll('mat-icon, [aria-hidden="true"]').forEach((n) => n.remove());
    text = (clone.textContent ?? '').replace(/\s+/g, ' ').trim();
  }
  if (text) return text;
  return el.getAttribute('title')?.trim() ?? null;
}

/**
 * What the person did (navigations and taps), sent to the server log beside
 * what the API saw, so "I pressed it and nothing happened" can be diagnosed.
 * `logging.ts` reports errors; this reports activity.
 *
 * Hooked in two places only, the router and one capture-phase click listener,
 * so no page needs to know about it. Sent with `fetch`, not `HttpClient`, so a
 * failing request cannot generate telemetry about itself through the
 * interceptors. A failed send is dropped.
 */
@Injectable({ providedIn: 'root' })
export class Telemetry {
  private readonly router = inject(Router);
  private readonly doc = inject(DOCUMENT);

  private queue: TelemetryEvent[] = [];
  private timer: ReturnType<typeof setInterval> | null = null;

  private static readonly FLUSH_MS = 5000;
  /** Queue length that forces a flush before the next tick. */
  private static readonly MAX_QUEUE = 50;

  /** Idempotent; called from the app shell. */
  init(): void {
    if (this.timer !== null) return;

    this.router.events
      .pipe(filter((e): e is NavigationEnd => e instanceof NavigationEnd))
      .subscribe((e) => this.enqueue('nav', e.urlAfterRedirects, null));

    // Capture phase, so a handler that stops propagation does not hide the tap.
    this.doc.addEventListener(
      'click',
      (ev) => {
        const label = labelFor(ev.target);
        if (label !== null) this.enqueue('tap', this.router.url, label);
      },
      { capture: true },
    );

    this.timer = setInterval(() => this.flush(false), Telemetry.FLUSH_MS);

    // Flush when hidden, in case the tab is closing.
    this.doc.addEventListener('visibilitychange', () => {
      if (this.doc.visibilityState === 'hidden') this.flush(true);
    });
  }

  private enqueue(kind: string, path: string, label: string | null): void {
    this.queue.push({ kind, path, label, at: Date.now() });
    if (this.queue.length >= Telemetry.MAX_QUEUE) this.flush(false);
  }

  private flush(final: boolean): void {
    if (this.queue.length === 0) return;
    const batch = this.queue;
    this.queue = [];
    // `sendBeacon` survives the page's teardown; a fetch may not.
    if (final && this.doc.defaultView?.navigator.sendBeacon) {
      this.doc.defaultView.navigator.sendBeacon(
        '/api/telemetry',
        new Blob([JSON.stringify(batch)], { type: 'application/json' }),
      );
      return;
    }
    try {
      void fetch('/api/telemetry', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(batch),
      }).catch(() => undefined);
    } catch {
      /* dropped */
    }
  }
}
