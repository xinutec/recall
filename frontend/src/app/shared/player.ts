import { Injectable, signal } from '@angular/core';

/** The app's one audio element: starting a clip stops the last, and tapping the
 * playing key again pauses in place. */
@Injectable({ providedIn: 'root' })
export class Player {
  readonly playing = signal<string | null>(null);
  /** Denoise server-side before playing (#1522); costs a few seconds per clip. */
  readonly enhance = signal(false);

  private readonly audio = new Audio();
  private loaded = '';
  private fallback: readonly string[] = [];
  private queue: readonly string[] = [];

  /** Play `url` under `key`, or pause it if `key` is playing. If `url` fails to
   * load, `fallback` plays in order instead: a span can cross two recordings. */
  toggle(key: string, url: string, fallback: readonly string[] = []): void {
    if (this.playing() === key) {
      this.audio.pause();
      this.playing.set(null);
      return;
    }
    if (this.loaded !== url) {
      this.load(url, fallback);
    }
    void this.audio.play();
    this.playing.set(key);
  }

  stop(): void {
    this.audio.pause();
    this.audio.src = '';
    this.loaded = '';
    this.playing.set(null);
  }

  /** `base` with the denoise flag when it is on. */
  clip(base: string): string {
    if (!this.enhance()) return base;
    return `${base}${base.includes('?') ? '&' : '?'}enhance=true`;
  }

  private load(url: string, fallback: readonly string[]): void {
    this.loaded = url;
    this.fallback = fallback;
    this.queue = [];
    this.audio.onended = () => this.next() || this.playing.set(null);
    this.audio.onerror = () => {
      // Only the first failure switches to the fallback; a failing fallback clip is skipped.
      if (this.fallback.length) {
        this.queue = this.fallback;
        this.fallback = [];
      }
      if (!this.next()) this.playing.set(null);
    };
    this.audio.src = url;
  }

  private next(): boolean {
    const [head, ...rest] = this.queue;
    if (head === undefined) return false;
    this.queue = rest;
    this.loaded = head;
    this.audio.src = head;
    void this.audio.play();
    return true;
  }
}
