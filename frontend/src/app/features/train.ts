import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  HostListener,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from '@angular/core';
import { toSignal } from '@angular/core/rxjs-interop';
import { Location } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { MatCardModule } from '@angular/material/card';
import { MatButtonModule } from '@angular/material/button';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatChipsModule } from '@angular/material/chips';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatTooltipModule } from '@angular/material/tooltip';
import { MatSnackBar } from '@angular/material/snack-bar';

import { Transcript } from '../models';
import { RecallApi } from '../recall-api';
import { formatClock, formatConfidence } from '../format';
import { ClipTrimmer } from './clip-trimmer';

// The household's languages, for fixing a mis-detected one (Dutch heard as English).
const LANGUAGES = ['nl', 'en'] as const;

/**
 * The labeling workstation: build the fine-tuning corpus one audible-but-
 * uncertain turn at a time. Play (loud, replay/slow), read the surrounding
 * context, tag who said it, fix the text, Save — or "Can't make out".
 */
@Component({
  selector: 'app-train',
  imports: [
    MatCardModule,
    MatButtonModule,
    MatFormFieldModule,
    MatInputModule,
    MatChipsModule,
    MatIconModule,
    MatProgressBarModule,
    MatTooltipModule,
    RouterLink,
    ClipTrimmer,
  ],
  templateUrl: './train.html',
  styleUrl: './train.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Train {
  private readonly api = inject(RecallApi);
  private readonly snack = inject(MatSnackBar);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly location = inject(Location);
  private readonly player = viewChild<ElementRef<HTMLAudioElement>>('player');
  private readonly fromInput = viewChild<ElementRef<HTMLInputElement>>('fromInput');
  private readonly toInput = viewChild<ElementRef<HTMLInputElement>>('toInput');

  // Quick-pick roster (keys 1-N) from runtime enrolment, not hard-coded — there is
  // deliberately no "Other" — every voice is a known person. Fetched so real names
  // stay out of the codebase.
  protected readonly speakers = signal<readonly string[]>([]);

  // URL is the source of truth for the time window (bookmarkable, reload-safe).
  // Read as a signal off queryParamMap rather than via route-input binding: this
  // drives an *imperative* queue load (not an httpResource), and queryParamMap
  // delivers the params synchronously on landing/Apply — so the windowed query
  // fetches once. Route inputs (withComponentInputBinding) bind a CD tick later,
  // which would fire a redundant unwindowed load first (train.integration.spec
  // pins this). The httpResource pages use input() because they're declarative.
  private readonly params = toSignal(this.route.queryParamMap);

  private readonly queue = signal<readonly Transcript[]>([]);
  private readonly index = signal(0);
  protected readonly corrections = signal(0);
  protected readonly bySpeaker = signal<Record<string, number>>({});
  protected readonly loading = signal(false);
  protected readonly busy = signal(false);
  protected readonly draft = signal('');
  protected readonly speaker = signal<string | null>(null);
  protected readonly languages = LANGUAGES;
  protected readonly language = signal<string>('nl');
  protected readonly before = signal<readonly Transcript[]>([]);
  protected readonly after = signal<readonly Transcript[]>([]);
  protected readonly suggested = signal<string | null>(null);
  protected readonly adjusting = signal(false);
  private readonly span = signal<{ start: string; end: string } | null>(null);
  protected readonly from = signal('');
  protected readonly to = signal('');
  protected readonly order = signal<'loudness' | 'time'>('loudness');
  protected readonly splitting = signal(false);
  protected readonly parts = signal<
    { start: string; end: string; text: string; speaker: string | null }[]
  >([]);

  protected readonly current = computed<Transcript | null>(
    () => this.queue()[this.index()] ?? null,
  );
  protected readonly remaining = computed(() => this.queue().length - this.index());
  protected readonly canGoBack = computed(() => this.index() > 0);
  /** Launched from the timeline to label one specific turn (?id=), vs the queue. */
  protected readonly targeted = computed(() => !!this.params()?.get('id'));
  protected readonly clock = (t: Transcript): string => formatClock(t.start);
  protected readonly confidence = (t: Transcript): string => formatConfidence(t.confidence);

  /** How many labelled fragments exist for a voice (drives the balance display). */
  protected readonly speakerCount = (name: string): number => this.bySpeaker()[name] ?? 0;

  /** Coarse audibility from measured loudness, so faint far-field clips (a mics
   * problem, not a labelling one) can be spotted and skipped at a glance. */
  protected readonly clarity = (t: Transcript): 'clear' | 'quiet' | 'faint' => {
    const l = t.loudness ?? 0;
    if (l >= 0.05) {
      return 'clear';
    }
    return l >= 0.01 ? 'quiet' : 'faint';
  };
  protected readonly dirty = computed(() => {
    const t = this.current();
    return t !== null && this.draft().trim() !== t.text.trim();
  });

  constructor() {
    this.api.speakers().subscribe({
      next: (r) => this.speakers.set(r.names),
      error: () => undefined,
    });
    // On each new turn: reset the draft + speaker, and load surrounding context.
    effect(() => {
      const t = this.current();
      this.draft.set(t ? t.text : '');
      // Pre-select the turn's existing label, so an already-tagged turn opens with
      // its speaker shown (ready to confirm or change) rather than blank. Unlabelled
      // turns stay null and fall through to the voiceprint suggestion below.
      this.speaker.set(t?.speakerConfirmed ? t.speaker : null);
      this.language.set(t?.language ?? 'nl');
      this.adjusting.set(false);
      this.span.set(null);
      this.splitting.set(false);
      this.parts.set([]);
      this.before.set([]);
      this.after.set([]);
      this.suggested.set(null);
      if (t) {
        // Both requests are guarded by the turn id they were issued for: a slow
        // reply landing after the user advanced must be dropped, or the PREVIOUS
        // turn's context/voiceprint suggestion would attach to the current one —
        // and one Enter press would save that wrong speaker into the corpus.
        const issuedFor = t.id;
        this.api.around(issuedFor).subscribe({
          next: (ctx) =>
            untracked(() => {
              if (this.current()?.id !== issuedFor) {
                return;
              }
              this.before.set(ctx.before);
              this.after.set(ctx.after);
            }),
          error: () => undefined,
        });
        // Ask who it sounds like; pre-select so Save is one tap — but never
        // override a choice the user has already made while it was in flight.
        // untracked: the callback can fire synchronously, and reading signals
        // here must not make them dependencies of an effect that also writes
        // them (that would self-retrigger into an infinite loop).
        this.api.suggest(issuedFor).subscribe({
          next: (s) =>
            untracked(() => {
              if (this.current()?.id !== issuedFor) {
                return;
              }
              if (s.speaker) {
                this.suggested.set(s.speaker);
                if (!this.speaker()) {
                  this.speaker.set(s.speaker);
                }
              }
            }),
          error: () => undefined,
        });
      }
    });
    // Drive the queue from the URL's ?from/?to/?order — runs on load, Apply,
    // back/forward. Seed the editable fields from the URL too, but the load reads
    // the URL values (not the field signals), so editing a field doesn't reload
    // until Apply commits it to the URL.
    effect(() => {
      const qp = this.params();
      const id = qp?.get('id');
      if (id) {
        // Labelling one specific turn, launched from the timeline (?id=). Skip the
        // ranked queue and load just that turn for the full trim/speaker/lang edit.
        this.loadOne(id);
        return;
      }
      const from = qp?.get('from') ?? '';
      const to = qp?.get('to') ?? '';
      const order = qp?.get('order') === 'time' ? 'time' : 'loudness';
      this.from.set(from);
      this.to.set(to);
      this.order.set(order);
      this.load(from, to, order);
    });
  }

  /** datetime-local strings are local time; toISOString() gives UTC for the API. */
  private toIso(local: string): string | undefined {
    if (!local) {
      return undefined;
    }
    const d = new Date(local);
    return Number.isNaN(d.getTime()) ? undefined : d.toISOString();
  }

  /** Write the window to the URL; the effect above reloads from it.
   *
   * Read the field values straight off the inputs at submit time rather than the
   * `from`/`to` signals: some mobile datetime-local pickers don't fire input or
   * change, so the signal can stay empty while the field visibly holds a value.
   * Pulling on submit applies whatever the field shows, events or not.
   */
  protected applyRange(): void {
    const fromVal = this.fromInput()?.nativeElement.value;
    const from = (fromVal !== undefined && fromVal !== '' ? fromVal : this.from()) || null;
    const toVal = this.toInput()?.nativeElement.value;
    const to = (toVal !== undefined && toVal !== '' ? toVal : this.to()) || null;
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { from, to },
      queryParamsHandling: 'merge', // keep the current order
    });
  }

  protected clearRange(): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { from: null, to: null },
      queryParamsHandling: 'merge', // keep the current order
    });
  }

  /** Flip the queue order; the URL drives the reload (default 'loudness' is
   * omitted so the URL stays clean). */
  protected setOrder(order: 'loudness' | 'time'): void {
    void this.router.navigate([], {
      relativeTo: this.route,
      queryParams: { order: order === 'time' ? 'time' : null },
      queryParamsHandling: 'merge',
    });
  }

  private load(fromLocal: string, toLocal: string, order: string): void {
    this.loading.set(true);
    this.api.trainQueue(40, this.toIso(fromLocal), this.toIso(toLocal), order).subscribe({
      next: (q) => {
        this.queue.set(q.items);
        this.corrections.set(q.corrections);
        this.bySpeaker.set(q.bySpeaker ?? {});
        this.index.set(0);
        this.loading.set(false);
      },
      error: () => {
        this.loading.set(false);
        this.snack.open('Could not load the training queue', 'Dismiss', { duration: 4000 });
      },
    });
  }

  /** Load a single turn to label (timeline → "label this one"). */
  private loadOne(id: string): void {
    this.loading.set(true);
    this.api.transcripts(id).subscribe({
      next: (r) => {
        this.queue.set(r.items);
        this.index.set(0);
        this.loading.set(false);
      },
      error: () => {
        this.loading.set(false);
        this.snack.open('Could not load that turn', 'Dismiss', { duration: 4000 });
      },
    });
  }

  protected edit(text: string): void {
    this.draft.set(text);
  }

  protected pickSpeaker(name: string): void {
    this.speaker.set(this.speaker() === name ? null : name);
  }

  protected setLanguage(lang: string): void {
    this.language.set(lang);
  }

  protected toggleAdjust(): void {
    this.adjusting.update((v) => !v);
  }

  protected onSpan(span: { start: string; end: string }): void {
    this.span.set(span);
  }

  /** Enter split mode: the trimmer is shown so each part gets its own span. */
  protected toggleSplit(): void {
    const on = !this.splitting();
    this.splitting.set(on);
    this.parts.set([]);
    this.adjusting.set(on);
  }

  /** Add the current (trimmed span + text + speaker) as one speaker's fragment. */
  protected addPart(): void {
    const sp = this.span();
    const text = this.draft().trim();
    if (!sp || !text) {
      this.snack.open('Trim the part and type its text first', undefined, { duration: 2500 });
      return;
    }
    this.parts.update((p) => [
      ...p,
      { start: sp.start, end: sp.end, text, speaker: this.speaker() },
    ]);
    this.draft.set('');
    this.speaker.set(null);
  }

  protected saveSplit(): void {
    const t = this.current();
    if (!t || this.busy()) {
      return;
    }
    // Fold in the part currently being edited, if any.
    const pending = this.span();
    const text = this.draft().trim();
    const all = [...this.parts()];
    if (pending && text) {
      all.push({ start: pending.start, end: pending.end, text, speaker: this.speaker() });
    }
    if (all.length < 2) {
      this.snack.open('A split needs at least two parts', undefined, { duration: 2500 });
      return;
    }
    this.busy.set(true);
    this.api
      .split(
        t.id,
        all.map((p) => ({
          start: p.start,
          end: p.end,
          text: p.text,
          ...(p.speaker ? { speaker: p.speaker } : {}),
        })),
      )
      .subscribe({
        next: (r) => {
          this.corrections.update((n) => n + r.newIds.length);
          all.forEach((p) => this.bumpSpeaker(p.speaker));
          this.busy.set(false);
          this.advance();
        },
        error: () => {
          this.busy.set(false);
          this.snack.open('Could not save the split', 'Dismiss', { duration: 4000 });
        },
      });
  }

  protected replay(rate = 1): void {
    const el = this.player()?.nativeElement;
    if (el) {
      el.playbackRate = rate;
      el.currentTime = 0;
      void el.play();
    }
  }

  protected save(): void {
    const t = this.current();
    const text = this.draft().trim();
    if (!t || !text || this.busy()) {
      return;
    }
    const sp = this.speaker();
    if (!sp) {
      // A fragment with no speaker can't teach the model whose voice it is —
      // the whole point here. Nudge instead of silently saving a weak label.
      this.snack.open(`Tag who said it first (keys 1–${this.speakers().length})`, undefined, {
        duration: 2500,
      });
      return;
    }
    this.busy.set(true);
    const opts: { speaker?: string; start?: string; end?: string; language?: string } = {
      speaker: sp,
      language: this.language(),
    };
    const span = this.adjusting() ? this.span() : null;
    if (span) {
      opts.start = span.start;
      opts.end = span.end;
    }
    this.api.correct(t.id, text, opts).subscribe({
      next: () => {
        this.corrections.update((n) => n + 1);
        this.bumpSpeaker(sp);
        this.busy.set(false);
        this.advance();
      },
      error: () => {
        this.busy.set(false);
        this.snack.open('Could not save', 'Dismiss', { duration: 4000 });
      },
    });
  }

  /** Optimistically bump the per-voice balance so it updates on Save, not only on
   * the next queue refetch. A reload reconciles it with the server count. */
  private bumpSpeaker(name: string | null): void {
    if (!name) {
      return;
    }
    this.bySpeaker.update((m) => ({ ...m, [name]: (m[name] ?? 0) + 1 }));
  }

  protected cantMakeOut(): void {
    const t = this.current();
    if (!t || this.busy()) {
      return;
    }
    this.busy.set(true);
    this.api.unintelligible(t.id).subscribe({
      next: () => {
        this.busy.set(false);
        this.advance();
      },
      error: () => {
        this.busy.set(false);
        this.snack.open('Could not flag', 'Dismiss', { duration: 4000 });
      },
    });
  }

  protected skip(): void {
    this.advance();
  }

  /** Step back to the previous clip in the batch — so a skip is reversible.
   * Clamped at the first; the per-turn effect re-loads that clip's context. */
  protected back(): void {
    if (this.busy() || this.index() === 0) {
      return;
    }
    this.index.update((i) => i - 1);
  }

  /** Keyboard shortcuts for fast labeling. 1-4 speaker · R replay · S skip ·
   * B back · X can't-make-out · Enter save (Cmd/Ctrl+Enter also works while typing). */
  @HostListener('document:keydown', ['$event'])
  protected onKey(e: KeyboardEvent): void {
    if (!this.current() || this.busy()) {
      return;
    }
    const save = (): void => (this.splitting() ? this.saveSplit() : this.save());
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      save();
      return;
    }
    const el = e.target as HTMLElement | null;
    if (el && (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT')) {
      return; // don't hijack typing
    }
    const roster = this.speakers();
    const speakerIdx = /^[1-9]$/.test(e.key) ? Number(e.key) - 1 : -1;
    if (speakerIdx >= 0 && speakerIdx < roster.length) {
      e.preventDefault();
      this.pickSpeaker(roster[speakerIdx]);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      save();
    } else if (e.key === 'r' || e.key === 'R') {
      e.preventDefault();
      this.replay(1);
    } else if (e.key === 's' || e.key === 'S') {
      e.preventDefault();
      this.skip();
    } else if (e.key === 'b' || e.key === 'B') {
      e.preventDefault();
      this.back();
    } else if (e.key === 'x' || e.key === 'X') {
      e.preventDefault();
      this.cantMakeOut();
    }
  }

  private advance(): void {
    if (this.targeted()) {
      // A one-off edit launched from the timeline — return there when done.
      this.location.back();
      return;
    }
    if (this.index() + 1 >= this.queue().length) {
      // Refill the same committed window + order — read from the URL, the source
      // of truth. The from/to field signals track every keystroke, so a refill
      // mid-edit would silently query a half-typed date.
      const qp = this.params();
      const order = qp?.get('order') === 'time' ? 'time' : 'loudness';
      this.load(qp?.get('from') ?? '', qp?.get('to') ?? '', order);
    } else {
      this.index.update((i) => i + 1);
    }
  }
}
