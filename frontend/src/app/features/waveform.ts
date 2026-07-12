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

import { reportToServer } from '../logging';
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

/** A pinch in progress: the geometry it started from, so the zoom is absolute rather than
 * accumulated frame by frame (which drifts). */
type Pinch = {
  readonly spread: number; // px between the fingers when it began
  readonly midX: number;
  readonly at: number; // the time under the midpoint, held still while zooming
  readonly span: number; // the window's duration when it began
} | null;

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
  // dev-lint: allow-component-list the sounds belong to *this span*, not to the app —
  // re-deriving them when the panel is reopened is the correct behaviour, and caching
  // them in a store would risk showing one span's sounds against another's audio.
  /** The sounds in the span itself, from a fetch that covered the whole of it. Zooming
   * and panning never touch this: the events are found on the same fine grid whatever
   * the zoom, so a window that covers the span is the definitive answer for it. */
  private readonly spanEvents = signal<readonly SoundEvent[]>([]);
  private readonly width = signal(0);
  private readonly playhead = signal<number | null>(null);
  protected readonly loading = signal(true);
  /** Time + level under the pointer — the readout that makes scrubbing legible. */
  protected readonly hover = signal<{ at: number; db: number | null } | null>(null);

  protected readonly height = HEIGHT_PX;
  /** Whether sound is actually coming out — set from the element's own `playing` event,
   * never from having asked it to play. */
  protected readonly playing = signal(false);
  /** Why nothing is coming out, when nothing is. Shown, not swallowed. */
  protected readonly problem = signal<string | null>(null);
  /** The segment the playhead is currently running through. */
  private origin: EnvelopeSegment | null = null;

  /**
   * Every audible thing inside the *selection* — what a delete would actually destroy.
   * Sounds in the padding either side are context (they're why the quiet ended), not
   * things to account for, so they're left out of the count.
   *
   * Taken from the span's own envelope, never the visible one. Panning away from the
   * span used to empty this list and the UI then said "no sound at all in this span" —
   * a reassurance about audio it simply wasn't looking at, in a view whose whole job is
   * approving a deletion.
   *
   * Ordered loudest first, which is the order that answers the only question that
   * matters here: is any of this speech? A span holds dozens of half-second crests of
   * the noise floor itself; speech would be the loud one. So the first few steps cover
   * the real risk, and the long quiet tail is there if you want it.
   */
  protected readonly events = computed(() =>
    [...this.spanEvents()]
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
  /** Fingers currently on the canvas, by pointer id → x. Two of them is a pinch. */
  private readonly pointers = new Map<number, number>();
  private pinch: Pinch = null;
  /** Whether this gesture ever became a pinch — so lifting out of one is not read as a
   * tap-to-play, even though the pinch itself ended with the first finger. */
  private pinched = false;
  private pending?: ReturnType<typeof setTimeout>;
  private inflight?: Subscription;
  private frame = 0;
  /** Every capture segment this waveform has ever loaded, by id. The span's own segments
   * all arrive in the first fetch (it covers the span plus context), so the selection is
   * complete from the outset and panning can only add to what is known, never remove. */
  private readonly known = new Map<number, EnvelopeSegment>();

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
    effect(() => this.listen(this.audioRef().nativeElement));

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
          // Remember every segment ever seen. A fetch only *adds* knowledge: what the
          // delete takes must not depend on where the view happens to be pointing.
          for (const segment of envelope.segments) {
            this.known.set(segment.audioId, segment);
          }
          // Likewise the sounds: only a window that covers the whole span can speak for
          // it. A narrower one knows less, and must not be allowed to say so.
          if (
            Date.parse(envelope.start) <= this.spanFrom() &&
            Date.parse(envelope.end) >= this.spanTo()
          ) {
            this.spanEvents.set(envelope.events);
          }
          this.envelope.set(envelope);
          this.loading.set(false);
          this.syncSelection();
        },
        error: () => this.loading.set(false),
      });
  }

  /**
   * The delete takes whole segments, so the selection means "every segment that lies
   * inside it" — a segment half-covered by a trimmed edge is kept, never guessed at.
   *
   * Derived from every segment seen, not from the ones currently on screen. Panning away
   * from part of the span used to drop those segments out of the delete: the count fell
   * from 100 to 85 just by dragging. Where you are looking is not what you are deleting.
   */
  private syncSelection(): void {
    const inside = [...this.known.values()]
      .filter(
        (s) =>
          Date.parse(s.start) >= this.selFrom() - 1 &&
          Date.parse(s.end) <= this.selTo() + 1,
      )
      .sort((a, b) => Date.parse(a.start) - Date.parse(b.start))
      .map((s) => s.audioId);
    this.selectedIds.set(inside);
  }

  private segments(): readonly EnvelopeSegment[] {
    return [...this.known.values()];
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
    this.pointers.set(event.pointerId, x);

    // A second finger down turns the gesture into a pinch, whatever it started as.
    if (this.pointers.size >= 2) {
      this.beginPinch();
      return;
    }
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
    if (this.pointers.has(event.pointerId)) {
      this.pointers.set(event.pointerId, x);
    }
    const at = this.timeAt(x);
    this.hover.set({ at, db: this.dbAt(at) });

    if (this.pinch) {
      this.updatePinch();
      return;
    }

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

  /**
   * Pinch to zoom. Zoom was bound to the mouse wheel, which a phone does not have — so on
   * the device this review is actually used on, the waveform was stuck at one fixed scale
   * and a half-second sound was a few pixels wide, and unreachable.
   *
   * The time under the midpoint of the two fingers is held still, so spreading magnifies
   * what is between them and sliding both fingers pans as well — one gesture, no modes.
   */
  private beginPinch(): void {
    const [a, b] = [...this.pointers.values()];
    this.drag = null; // whatever the first finger started, two fingers is a pinch
    this.pinched = true;
    this.pinch = {
      spread: Math.abs(a - b),
      midX: (a + b) / 2,
      at: this.timeAt((a + b) / 2),
      span: this.viewTo() - this.viewFrom(),
    };
  }

  private updatePinch(): void {
    const pinch = this.pinch;
    const [a, b] = [...this.pointers.values()];
    if (!pinch || a === undefined || b === undefined) {
      return;
    }
    const spread = Math.abs(a - b);
    if (spread < 1 || pinch.spread < 1) {
      return;
    }
    const span = Math.min(
      Math.max((pinch.span * pinch.spread) / spread, MIN_WINDOW_MS),
      MAX_WINDOW_MS,
    );
    // Anchor the time that was under the midpoint to wherever the midpoint is now.
    const midX = (a + b) / 2;
    const ratio = midX / Math.max(this.width(), 1);
    this.viewFrom.set(pinch.at - span * ratio);
    this.viewTo.set(pinch.at + span * (1 - ratio));
  }

  /** Back to the whole span, framed. There was no way back: pan far enough and the span
   * you were judging was simply gone, with nothing to steer by. */
  protected fitSpan(): void {
    const pad = Math.max((this.spanTo() - this.spanFrom()) * PAD_RATIO, MIN_PAD_MS);
    this.viewFrom.set(this.spanFrom() - pad);
    this.viewTo.set(this.spanTo() + pad);
  }

  protected onPointerUp(event: PointerEvent): void {
    this.pointers.delete(event.pointerId);
    this.canvasRef().nativeElement.releasePointerCapture(event.pointerId);

    if (this.pointers.size < 2) {
      this.pinch = null;
    }
    if (this.pointers.size > 0) {
      return; // a finger is still down; the gesture isn't over
    }

    const drag = this.drag;
    // `pinched` outlives the pinch itself: the first finger up already ended it, and the
    // second must still not be read as a tap. It clears only when the hand is off.
    const pinched = this.pinched;
    this.drag = null;
    this.pinched = false;

    if (
      !pinched &&
      drag?.kind === 'pan' &&
      Math.abs(this.localX(event) - drag.x) < CLICK_SLOP_PX
    ) {
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

  /**
   * Play from an absolute time: seek into the segment covering it, then run on through
   * the following segments — the point is to *hear* the edge, which straddles files.
   *
   * Nothing here declares itself to be playing. `playing` is driven by the audio element
   * (see `listen`), because the previous version set it to true the moment it *asked* for
   * playback and dropped the returned promise on the floor. On the phone, playback that
   * was rejected still lit the Stop button: the UI claimed sound was coming out when none
   * was. A control that lies about a deletion review is worse than one that fails.
   */
  private playFrom(at: number): void {
    const segment = this.segments().find(
      (s) => Date.parse(s.start) <= at && at < Date.parse(s.end),
    );
    if (!segment) {
      // No audio here (a gap, or beyond what's loaded). Say so; don't sit there mute.
      this.stop();
      this.problem.set('nothing recorded at that point');
      return;
    }
    const audio = this.audioRef().nativeElement;
    const offset = (at - Date.parse(segment.start)) / 1000;
    this.problem.set(null);
    this.origin = segment;

    const start = (): void => {
      audio.currentTime = offset;
      audio.play().catch((error: unknown) => this.failed(segment, error));
    };
    if (audio.dataset['audioId'] === String(segment.audioId) && audio.readyState > 0) {
      start();
    } else {
      audio.dataset['audioId'] = String(segment.audioId);
      audio.src = this.api.quietAudioUrl(segment.audioId);
      audio.addEventListener('loadedmetadata', start, { once: true });
      audio.load(); // the WebView will not always fetch on src alone
    }
  }

  /** Bind the UI to what the audio element is *actually* doing. The phone has no console,
   * so a refusal to play is also reported to the server (logs/client.log) — otherwise a
   * silent Play button leaves no trace anywhere. */
  private listen(audio: HTMLAudioElement): void {
    audio.addEventListener('playing', () => {
      this.playing.set(true);
      this.problem.set(null);
      this.follow();
    });
    audio.addEventListener('pause', () => this.playing.set(false));
    audio.addEventListener('ended', () => {
      const from = this.origin;
      if (from) {
        this.playFrom(Date.parse(from.end) + 1); // roll into the next segment
      }
    });
    audio.addEventListener('error', () =>
      this.failed(this.origin, audio.error?.message ?? 'media error'),
    );
    audio.addEventListener('stalled', () =>
      this.problem.set('audio stalled — the segment is not arriving'),
    );
  }

  private failed(segment: EnvelopeSegment | null, error: unknown): void {
    const detail = error instanceof Error ? error.message : String(error);
    this.playing.set(false);
    this.playhead.set(null);
    this.problem.set(`could not play: ${detail}`);
    reportToServer(
      'audio',
      `waveform play failed (segment ${segment?.audioId ?? '?'}): ${detail}`,
    );
  }

  /** Follow the playhead while sound is actually coming out. Started from the `playing`
   * event, not from the request to play: on a fresh segment the element is still loading
   * then, so the old version's loop saw `paused` and exited before the first frame. */
  private follow(): void {
    cancelAnimationFrame(this.frame);
    const audio = this.audioRef().nativeElement;
    const tick = (): void => {
      const from = this.origin;
      if (audio.paused || !from) {
        return;
      }
      this.playhead.set(Date.parse(from.start) + audio.currentTime * 1000);
      this.frame = requestAnimationFrame(tick);
    };
    this.frame = requestAnimationFrame(tick);
  }

  protected stop(): void {
    const audio = this.audioRef().nativeElement;
    audio.pause();
    cancelAnimationFrame(this.frame);
    this.origin = null;
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
    if (!event) {
      return;
    }
    this.bringIntoView(Date.parse(event.start), Date.parse(event.end));
    this.playFrom(Date.parse(event.start) - EVENT_LEAD_IN_MS);
  }

  /** Scroll the view to a sound that isn't on screen. Hearing something you cannot see
   * is no way to judge it — and after panning, the next sound is usually elsewhere. */
  private bringIntoView(from: number, to: number): void {
    if (from >= this.viewFrom() && to <= this.viewTo()) {
      return;
    }
    const span = this.viewTo() - this.viewFrom();
    const middle = (from + to) / 2;
    this.viewFrom.set(middle - span / 2);
    this.viewTo.set(middle + span / 2);
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

    // The playhead is the only pure *neutral* on this canvas — primary is the handles and
    // the loud bars, tertiary the sound being auditioned, error a gap. Colour here would
    // read as a *kind* of thing; it needs to read as *where you are*. So it earns its
    // legibility from contrast, not hue: a halo in the background colour underneath it,
    // so the line holds up over pale bars and the tinted selection band alike, and a cap
    // at the top so the eye finds it while it moves.
    const head = this.playhead();
    if (head !== null) {
      const x = Math.round(this.xOf(head)) + 0.5; // crisp on the pixel grid, not blurred
      ctx.strokeStyle = this.style('--wave-playhead-halo');
      ctx.lineWidth = 4;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, HEIGHT_PX);
      ctx.stroke();

      ctx.strokeStyle = this.style('--wave-playhead');
      ctx.fillStyle = ctx.strokeStyle;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x, HEIGHT_PX);
      ctx.stroke();
      ctx.fillRect(x - 3.5, 0, 7, 4);
    }
  }
}
