import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  OnDestroy,
  effect,
  input,
  output,
  signal,
  viewChild,
} from '@angular/core';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';

import { Transcript } from '../models';

interface Span {
  readonly start: string;
  readonly end: string;
}

const NUDGE_S = 0.1;
const LEAD = 1.5;
const TAIL = 1.5;
const PEAKS = 220;

/**
 * Trim a clip to exactly one speaker: shows the turn ± context as a waveform,
 * lets you nudge the start/end and play the selection, and emits the adjusted
 * absolute span so the correction is saved tightly aligned to the audio.
 */
@Component({
  selector: 'app-clip-trimmer',
  imports: [MatButtonModule, MatIconModule],
  templateUrl: './clip-trimmer.html',
  styleUrl: './clip-trimmer.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class ClipTrimmer implements OnDestroy {
  readonly turn = input.required<Transcript>();
  readonly spanChange = output<Span>();

  private readonly canvasRef = viewChild<ElementRef<HTMLCanvasElement>>('canvas');

  protected readonly selStart = signal(0);
  protected readonly selEnd = signal(0);
  protected readonly ready = signal(false);
  /** The clip couldn't be fetched/decoded — surface it instead of failing silently. */
  protected readonly failed = signal(false);

  private duration = 0;
  private leadActual = LEAD;
  private peaks: number[] = [];
  // The decoded clip, kept so the selection plays straight from the buffer
  // (Web Audio) instead of seeking an <audio> element — mobile seeking is
  // unreliable and would start playback from the file start.
  private buffer: AudioBuffer | null = null;
  private playCtx: AudioContext | null = null;
  private source: AudioBufferSourceNode | null = null;

  constructor() {
    effect(() => {
      const t = this.turn();
      void this.load(t);
    });
  }

  protected get selDuration(): number {
    return Math.max(0, this.selEnd() - this.selStart());
  }

  private async load(t: Transcript): Promise<void> {
    this.ready.set(false);
    this.failed.set(false);
    try {
      const url = `/api/clip/${t.id}?lead=${LEAD}&tail=${TAIL}`;
      const res = await fetch(url);
      if (!res.ok) {
        throw new Error(`clip fetch failed: ${res.status}`);
      }
      this.leadActual = Number.parseFloat(res.headers.get('X-Lead') ?? `${LEAD}`);
      const bytes = await res.arrayBuffer();
      const ctx = new AudioContext();
      const buf = await ctx.decodeAudioData(bytes.slice(0));
      void ctx.close();
      this.buffer = buf;
      this.duration = buf.duration;
      this.peaks = this.computePeaks(buf);
      const turnDur = (new Date(t.end).getTime() - new Date(t.start).getTime()) / 1000;
      this.selStart.set(this.leadActual);
      this.selEnd.set(Math.min(this.duration, this.leadActual + turnDur));
      this.ready.set(true);
      this.draw();
      this.emit();
    } catch {
      // A failed clip fetch/decode must not reject unhandled — show it instead.
      this.failed.set(true);
    }
  }

  private computePeaks(buf: AudioBuffer): number[] {
    const data = buf.getChannelData(0);
    const block = Math.floor(data.length / PEAKS) || 1;
    const peaks: number[] = [];
    for (let i = 0; i < PEAKS; i++) {
      let max = 0;
      for (let j = 0; j < block; j++) {
        max = Math.max(max, Math.abs(data[i * block + j] ?? 0));
      }
      peaks.push(max);
    }
    return peaks;
  }

  private draw(): void {
    const canvas = this.canvasRef()?.nativeElement;
    if (!canvas || this.duration <= 0) {
      return;
    }
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      return;
    }
    const w = (canvas.width = canvas.clientWidth);
    const h = (canvas.height = canvas.clientHeight);
    ctx.clearRect(0, 0, w, h);
    const styles = getComputedStyle(canvas);
    const accent = styles.getPropertyValue('--mat-sys-primary') || '#7cc';
    const dim = 'rgba(127,127,127,0.45)';
    // selection shading
    const sx = (this.selStart() / this.duration) * w;
    const ex = (this.selEnd() / this.duration) * w;
    ctx.fillStyle = 'rgba(124,200,200,0.15)';
    ctx.fillRect(sx, 0, ex - sx, h);
    // waveform bars: in-selection accent, outside dim
    const bw = w / this.peaks.length;
    this.peaks.forEach((p, i) => {
      const x = i * bw;
      const t = (i / this.peaks.length) * this.duration;
      ctx.fillStyle = t >= this.selStart() && t <= this.selEnd() ? accent : dim;
      const bh = Math.max(1, p * h * 0.95);
      ctx.fillRect(x, (h - bh) / 2, Math.max(1, bw - 1), bh);
    });
  }

  protected nudge(which: 'start' | 'end', dir: 1 | -1): void {
    const delta = dir * NUDGE_S;
    if (which === 'start') {
      this.selStart.set(Math.max(0, Math.min(this.selEnd() - 0.1, this.selStart() + delta)));
    } else {
      this.selEnd.set(
        Math.min(this.duration, Math.max(this.selStart() + 0.1, this.selEnd() + delta)),
      );
    }
    this.draw();
    this.emit();
  }

  /** Play exactly the selected span straight from the decoded buffer (Web Audio).
   * `start(when, offset, duration)` is sample-accurate — no seeking, so it always
   * begins at the selection start, including on mobile. */
  protected playSelection(): void {
    const buffer = this.buffer;
    if (!buffer) {
      return;
    }
    this.stopPlayback();
    const ctx = (this.playCtx ??= new AudioContext());
    void ctx.resume(); // iOS resumes the context on the tap gesture
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(ctx.destination);
    src.start(0, this.selStart(), this.selDuration);
    this.source = src;
  }

  private stopPlayback(): void {
    try {
      this.source?.stop();
    } catch {
      /* a finished/never-started source throws on stop(); ignore */
    }
    this.source = null;
  }

  ngOnDestroy(): void {
    this.stopPlayback();
    void this.playCtx?.close();
  }

  private emit(): void {
    const t = this.turn();
    const winStartMs = new Date(t.start).getTime() - this.leadActual * 1000;
    this.spanChange.emit({
      start: new Date(winStartMs + this.selStart() * 1000).toISOString(),
      end: new Date(winStartMs + this.selEnd() * 1000).toISOString(),
    });
  }
}
