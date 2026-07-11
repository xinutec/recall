import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  effect,
  inject,
  input,
  model,
  signal,
  viewChild,
} from '@angular/core';
import { DatePipe, DecimalPipe } from '@angular/common';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';
import { MatTooltipModule } from '@angular/material/tooltip';
import { Subscription } from 'rxjs';

import { Envelope, EnvelopeSegment, SoundEvent } from '../models';
import { RecallApi } from '../recall-api';

/** dB range drawn. The mic's noise floor sits near -62, so a scale that bottoms out at
 * -95 leaves the floor visible as a low band rather than a flat line on the axis. */
const MIN_DB = -95;
const MAX_DB = -10;

const HEIGHT_PX = 132;
/** One bar per screen pixel — asking for more just wastes decode. */
const MAX_POINTS = 2000;
/** Context around the span, so what ended the quiet is on screen without panning. */
const PAD_RATIO = 0.15;
const MIN_PAD_MS = 60_000;
const MIN_WINDOW_MS = 5_000;
const MAX_WINDOW_MS = 6 * 3600_000;
/** Pointer slop below which a drag is really a click (play from here). */
const CLICK_SLOP_PX = 4;
const HANDLE_GRAB_PX = 8;
/** Zoom/pan re-decodes server-side; coalesce a flurry of wheel events into one request. */
const REFETCH_DEBOUNCE_MS = 120;
/** Start playback just before a sound, so it isn't clipped by its own onset. */
const EVENT_LEAD_IN_MS = 700;

type Drag =
  | { readonly kind: 'pan'; readonly x: number; readonly start: number }
  | { readonly kind: 'trim-start' | 'trim-end' }
  | null;

/**
 * The waveform of one capture source, drawn so a quiet span can actually be judged:
 * every sound above the threshold stands out, the span under review is highlighted, and
 * the segments either side of it are on screen — so *why the quiet ends* is visible, not
 * inferred. Wheel zooms, drag pans, click plays from that point, and the edge handles
 * trim the span before it is deleted.
 *
 * The bars are peaks, never averages (see recall.envelope): zooming out may not hide a
 * short sound in a view whose whole purpose is approving a deletion.
 */
@Component({
  selector: 'app-waveform',
  imports: [
    DatePipe,
    DecimalPipe,
    MatButtonModule,
    MatIconModule,
    MatTooltipModule,
  ],
  templateUrl: './waveform.html',
  styleUrl: './waveform.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Waveform {
  private readonly api = inject(RecallApi);
  private readonly destroyRef = inject(DestroyRef);

  readonly source = input.required<string>();
  readonly spanStart = input.required<string>();
  readonly spanEnd = input.required<string>();
  /** The segments the delete will take — narrowed when the span's edges are trimmed. */
  readonly selectedIds = model.required<readonly number[]>();

  private readonly canvasRef =
    viewChild.required<ElementRef<HTMLCanvasElement>>('canvas');
  private readonly audioRef =
    viewChild.required<ElementRef<HTMLAudioElement>>('player');

  private readonly spanFrom = computed(() => Date.parse(this.spanStart()));
  private readonly spanTo = computed(() => Date.parse(this.spanEnd()));

  // The visible window (epoch ms), and the selection inside it (what delete acts on).
  private readonly viewFrom = signal(0);
  private readonly viewTo = signal(0);
  private readonly selFrom = signal(0);
  private readonly selTo = signal(0);

  private readonly envelope = signal<Envelope | null>(null);
  private readonly width = signal(0);
  private readonly playhead = signal<number | null>(null);
  protected readonly loading = signal(true);
  /** Time + level under the pointer — the readout that makes scrubbing legible. */
  protected readonly hover = signal<{ at: number; db: number | null } | null>(null);

  protected readonly height = HEIGHT_PX;
  protected readonly playing = signal(false);

  /**
   * Every audible thing inside the *selection* — what a delete would actually destroy.
   * Sounds in the padding either side are context (they're why the quiet ended), not
   * things to account for, so they're left out of the count.
   *
   * Ordered loudest first, which is the order that answers the only question that
   * matters here: is any of this speech? A span holds dozens of half-second crests of
   * the noise floor itself; speech would be the loud one. So the first few steps cover
   * the real risk, and the long quiet tail is there if you want it.
   */
  protected readonly events = computed(() =>
    [...(this.envelope()?.events ?? [])]
      .filter(
        (e) =>
          Date.parse(e.start) >= this.selFrom() && Date.parse(e.end) <= this.selTo(),
      )
      .sort((a, b) => b.peakDb - a.peakDb),
  );
  protected readonly cursor = signal(0);
  protected readonly current = computed<SoundEvent | undefined>(
    () => this.events()[this.cursor()],
  );

  protected readonly trimmed = computed(
    () => this.selFrom() !== this.spanFrom() || this.selTo() !== this.spanTo(),
  );
  protected readonly selectionLabel = computed(() => {
    const seconds = Math.round((this.selTo() - this.selFrom()) / 1000);
    const mins = Math.floor(seconds / 60);
    return `${mins}m ${String(seconds % 60).padStart(2, '0')}s`;
  });

  private drag: Drag = null;
  private pending?: ReturnType<typeof setTimeout>;
  private inflight?: Subscription;
  private frame = 0;

  constructor() {
    effect(() => {
      const pad = Math.max((this.spanTo() - this.spanFrom()) * PAD_RATIO, MIN_PAD_MS);
      this.viewFrom.set(this.spanFrom() - pad);
      this.viewTo.set(this.spanTo() + pad);
      this.selFrom.set(this.spanFrom());
      this.selTo.set(this.spanTo());
    });

    // Window moved → re-decode (debounced; the server caches per segment, so panning
    // back over ground already seen returns immediately).
    effect(() => {
      const [from, to, source] = [this.viewFrom(), this.viewTo(), this.source()];
      if (to <= from) {
        return;
      }
      clearTimeout(this.pending);
      this.pending = setTimeout(() => this.fetch(source, from, to), REFETCH_DEBOUNCE_MS);
    });

    effect(() => this.draw());

    const observer = new ResizeObserver(([entry]) =>
      this.width.set(Math.round(entry.contentRect.width)),
    );
    effect(() => observer.observe(this.canvasRef().nativeElement));

    this.destroyRef.onDestroy(() => {
      observer.disconnect();
      clearTimeout(this.pending);
      this.inflight?.unsubscribe();
      cancelAnimationFrame(this.frame);
    });
  }

  private fetch(source: string, from: number, to: number): void {
    this.inflight?.unsubscribe();
    this.loading.set(true);
    this.inflight = this.api
      .quietEnvelope(source, new Date(from), new Date(to), MAX_POINTS)
      .subscribe({
        next: (envelope) => {
          this.envelope.set(envelope);
          this.loading.set(false);
          this.syncSelection();
        },
        error: () => this.loading.set(false),
      });
  }

  /** The delete takes whole segments, so the selection means "every segment that lies
   * inside it" — a segment half-covered by a trimmed edge is kept, never guessed at. */
  private syncSelection(): void {
    const inside = this.segments()
      .filter(
        (s) =>
          Date.parse(s.start) >= this.selFrom() - 1 &&
          Date.parse(s.end) <= this.selTo() + 1,
      )
      .map((s) => s.audioId);
    this.selectedIds.set(inside);
  }

  private segments(): readonly EnvelopeSegment[] {
    return this.envelope()?.segments ?? [];
  }

  // ---- geometry -----------------------------------------------------------------

  private xOf(at: number): number {
    const [from, to] = [this.viewFrom(), this.viewTo()];
    return ((at - from) / (to - from)) * this.width();
  }

  private timeAt(x: number): number {
    const [from, to] = [this.viewFrom(), this.viewTo()];
    return from + (x / Math.max(this.width(), 1)) * (to - from);
  }

  private dbAt(at: number): number | null {
    const envelope = this.envelope();
    if (!envelope) {
      return null;
    }
    const index = Math.floor(
      (at - Date.parse(envelope.start)) / 1000 / envelope.bucketS,
    );
    return envelope.points[index] ?? null;
  }

  // ---- interaction --------------------------------------------------------------

  protected onWheel(event: WheelEvent): void {
    event.preventDefault();
    const [from, to] = [this.viewFrom(), this.viewTo()];
    const at = this.timeAt(this.localX(event));
    const scale = Math.exp(event.deltaY * 0.002);
    const span = Math.min(
      Math.max((to - from) * scale, MIN_WINDOW_MS),
      MAX_WINDOW_MS,
    );
    // Zoom about the pointer, so the sound you're inspecting stays under the cursor.
    const ratio = (at - from) / (to - from);
    this.viewFrom.set(at - span * ratio);
    this.viewTo.set(at + span * (1 - ratio));
  }

  protected onPointerDown(event: PointerEvent): void {
    const x = this.localX(event);
    this.canvasRef().nativeElement.setPointerCapture(event.pointerId);
    if (Math.abs(x - this.xOf(this.selFrom())) <= HANDLE_GRAB_PX) {
      this.drag = { kind: 'trim-start' };
    } else if (Math.abs(x - this.xOf(this.selTo())) <= HANDLE_GRAB_PX) {
      this.drag = { kind: 'trim-end' };
    } else {
      this.drag = { kind: 'pan', x, start: this.viewFrom() };
    }
  }

  protected onPointerMove(event: PointerEvent): void {
    const x = this.localX(event);
    const at = this.timeAt(x);
    this.hover.set({ at, db: this.dbAt(at) });

    const drag = this.drag;
    if (!drag) {
      return;
    }
    if (drag.kind === 'pan') {
      if (Math.abs(x - drag.x) < CLICK_SLOP_PX) {
        return;
      }
      const span = this.viewTo() - this.viewFrom();
      const shifted = drag.start - ((x - drag.x) / Math.max(this.width(), 1)) * span;
      this.viewFrom.set(shifted);
      this.viewTo.set(shifted + span);
      return;
    }
    // Trim: the edges may only pull inward — a span can be narrowed to protect audio,
    // never widened to sweep in segments the detector didn't propose.
    if (drag.kind === 'trim-start') {
      this.selFrom.set(Math.min(Math.max(at, this.spanFrom()), this.selTo()));
    } else {
      this.selTo.set(Math.max(Math.min(at, this.spanTo()), this.selFrom()));
    }
    this.syncSelection();
  }

  protected onPointerUp(event: PointerEvent): void {
    const drag = this.drag;
    this.drag = null;
    this.canvasRef().nativeElement.releasePointerCapture(event.pointerId);
    if (drag?.kind === 'pan' && Math.abs(this.localX(event) - drag.x) < CLICK_SLOP_PX) {
      this.playFrom(this.timeAt(this.localX(event)));
    }
  }

  protected onPointerLeave(): void {
    this.hover.set(null);
  }

  protected resetTrim(): void {
    this.selFrom.set(this.spanFrom());
    this.selTo.set(this.spanTo());
    this.syncSelection();
  }

  // ---- playback -----------------------------------------------------------------

  /** Play from an absolute time: seek into the segment covering it, then run on through
   * the following segments — the point is to *hear* the edge, which straddles files. */
  private playFrom(at: number): void {
    const segment = this.segments().find(
      (s) => Date.parse(s.start) <= at && at < Date.parse(s.end),
    );
    if (!segment) {
      this.stop();
      return;
    }
    const audio = this.audioRef().nativeElement;
    const offset = (at - Date.parse(segment.start)) / 1000;
    const url = this.api.quietAudioUrl(segment.audioId);
    const seek = (): void => {
      audio.currentTime = offset;
      void audio.play();
    };
    if (audio.dataset['audioId'] === String(segment.audioId)) {
      seek();
    } else {
      audio.dataset['audioId'] = String(segment.audioId);
      audio.src = url;
      audio.addEventListener('loadedmetadata', seek, { once: true });
    }
    this.playing.set(true);
    this.track(segment);
  }

  /** Follow the playhead in real time, and roll into the next segment when one ends. */
  private track(segment: EnvelopeSegment): void {
    cancelAnimationFrame(this.frame);
    const audio = this.audioRef().nativeElement;
    const tick = (): void => {
      if (audio.paused) {
        return;
      }
      this.playhead.set(Date.parse(segment.start) + audio.currentTime * 1000);
      this.frame = requestAnimationFrame(tick);
    };
    audio.onended = () => this.playFrom(Date.parse(segment.end) + 1);
    this.frame = requestAnimationFrame(tick);
  }

  protected stop(): void {
    const audio = this.audioRef().nativeElement;
    audio.pause();
    audio.onended = null;
    cancelAnimationFrame(this.frame);
    this.playing.set(false);
    this.playhead.set(null);
  }

  protected playSpan(): void {
    this.playFrom(this.selFrom());
  }

  /** Step to a sound and play it with a moment's lead-in, so it isn't clipped and you
   * can tell a cough from a word. Stepping is the point: every sound gets heard. */
  protected step(delta: number): void {
    const count = this.events().length;
    if (count === 0) {
      return;
    }
    this.cursor.set((this.cursor() + delta + count) % count);
    this.playEvent();
  }

  protected playEvent(): void {
    const event = this.current();
    if (event) {
      this.playFrom(Date.parse(event.start) - EVENT_LEAD_IN_MS);
    }
  }

  private localX(event: PointerEvent | WheelEvent): number {
    return event.clientX - this.canvasRef().nativeElement.getBoundingClientRect().left;
  }

  // ---- drawing ------------------------------------------------------------------

  private style(name: string): string {
    return getComputedStyle(this.canvasRef().nativeElement)
      .getPropertyValue(name)
      .trim();
  }

  private draw(): void {
    const canvas = this.canvasRef().nativeElement;
    const width = this.width();
    const envelope = this.envelope();
    const ratio = devicePixelRatio || 1;
    canvas.width = width * ratio;
    canvas.height = HEIGHT_PX * ratio;
    const ctx = canvas.getContext('2d');
    if (!ctx || !width) {
      return;
    }
    ctx.scale(ratio, ratio);
    ctx.clearRect(0, 0, width, HEIGHT_PX);
    if (!envelope) {
      return;
    }

    const threshold = envelope.thresholdDb;
    const yOf = (db: number): number =>
      HEIGHT_PX * (1 - (Math.min(Math.max(db, MIN_DB), MAX_DB) - MIN_DB) / (MAX_DB - MIN_DB));
    const from = Date.parse(envelope.start);
    const step = envelope.bucketS * 1000;

    // The selection: what a delete would take. Everything outside it is dimmed, so the
    // eye goes to the audio actually at stake.
    ctx.fillStyle = this.style('--wave-selection');
    ctx.fillRect(
      this.xOf(this.selFrom()),
      0,
      this.xOf(this.selTo()) - this.xOf(this.selFrom()),
      HEIGHT_PX,
    );

    const quiet = this.style('--wave-quiet');
    const loud = this.style('--wave-loud');
    const gap = this.style('--wave-gap');
    envelope.points.forEach((db, i) => {
      const x = this.xOf(from + i * step);
      const w = Math.max(this.xOf(from + (i + 1) * step) - x, 1);
      if (db === null) {
        // No audio here — a hole in the recording, not silence. Drawn as a band so it
        // can never be mistaken for a quiet stretch that's safe to delete.
        ctx.fillStyle = gap;
        ctx.fillRect(x, 0, w, HEIGHT_PX);
        return;
      }
      const y = yOf(db);
      ctx.fillStyle = db > threshold ? loud : quiet;
      ctx.fillRect(x, y, w, HEIGHT_PX - y);
    });

    // The threshold every span is judged against: bars poking above it are why a quiet
    // run ended.
    ctx.strokeStyle = this.style('--wave-threshold');
    ctx.setLineDash([4, 4]);
    ctx.beginPath();
    ctx.moveTo(0, yOf(threshold));
    ctx.lineTo(width, yOf(threshold));
    ctx.stroke();
    ctx.setLineDash([]);

    // Mark the sound being auditioned, so the list and the picture agree on which one
    // "this" is.
    const event = this.current();
    if (event) {
      const x = this.xOf(Date.parse(event.start));
      const w = Math.max(this.xOf(Date.parse(event.end)) - x, 2);
      ctx.fillStyle = this.style('--wave-event');
      ctx.fillRect(x - 1, 0, w + 2, HEIGHT_PX);
    }

    ctx.strokeStyle = this.style('--wave-handle');
    ctx.lineWidth = 2;
    for (const edge of [this.selFrom(), this.selTo()]) {
      ctx.beginPath();
      ctx.moveTo(this.xOf(edge), 0);
      ctx.lineTo(this.xOf(edge), HEIGHT_PX);
      ctx.stroke();
    }

    const head = this.playhead();
    if (head !== null) {
      ctx.strokeStyle = this.style('--wave-playhead');
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(this.xOf(head), 0);
      ctx.lineTo(this.xOf(head), HEIGHT_PX);
      ctx.stroke();
    }
  }
}
