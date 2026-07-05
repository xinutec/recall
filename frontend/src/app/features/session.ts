import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  OnDestroy,
  signal,
} from '@angular/core';
import { httpResource } from '@angular/common/http';
import { RouterLink } from '@angular/router';
import { FormsModule } from '@angular/forms';
import { MatButtonModule } from '@angular/material/button';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';
import { MatFormFieldModule } from '@angular/material/form-field';
import { MatInputModule } from '@angular/material/input';
import { MatSnackBar } from '@angular/material/snack-bar';

import { ConversationPage, SpeakerNames, Transcript, VoiceSuggestions } from '../models';
import { RecallApi } from '../recall-api';
import { resolveSelection, SpanSel } from '../selection-span';
import { TranscriptCard } from '../shared/transcript-card';
import { dayLabel, timeOfDay, timeOfDaySeconds } from '../format';

/** A diarization voice in this session: its cluster id, the name it's been given (if
 * any), and how many turns it holds — what the top naming strip renders. */
interface Voice {
  readonly cluster: string;
  readonly name: string | null;
  readonly turns: number;
  readonly sample: string; // a representative snippet, so you can tell who this voice is
  readonly sampleUrl: string; // and hear it
  readonly suggested: string | null; // voiceprint-based name suggestion, if confident
}

/** Consecutive turns by one speaker, read as a single paragraph. The underlying turns
 * stay separate (each a tappable span) so a selection can split within the run. */
interface Run {
  readonly key: number;
  readonly speaker: string;
  readonly confirmed: boolean;
  readonly start: string;
  readonly turns: readonly Transcript[];
}

/** One session's full transcript with annotation: name each diarization voice once
 * (applies to all its turns) and correct/reassign individual turns. Editing is only
 * offered once the whole session has settled (diarized/corrected), so edits land on
 * the final version, never an intermittent one that's about to be superseded. */
@Component({
  selector: 'app-session',
  imports: [
    RouterLink,
    FormsModule,
    MatButtonModule,
    MatIconModule,
    MatProgressBarModule,
    MatFormFieldModule,
    MatInputModule,
    TranscriptCard,
  ],
  templateUrl: './session.html',
  styleUrl: './session.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Session implements OnDestroy {
  /** The session's source id, bound from the /sessions/:id route. */
  readonly id = input.required<string>();

  private readonly snack = inject(MatSnackBar);
  private readonly api = inject(RecallApi);

  // High limit: a session is bounded (a single recording), so one fetch holds it all.
  protected readonly data = httpResource<ConversationPage>(() => {
    const params = new URLSearchParams({ source: this.id(), limit: '5000' });
    return `/api/conversations?${params.toString()}`;
  });
  // Known speaker names for autocomplete (enrolled voices + labels assigned anywhere),
  // refreshed after naming so a name typed here immediately suggests on the others.
  private readonly speakerNames = httpResource<SpeakerNames>(() => '/api/speakers');
  protected readonly knownNames = computed(() => this.speakerNames.value()?.names ?? []);

  // Auto-suggested name per voice (cluster) from voiceprint guesses — the enrolled
  // household member is identified for us; the clinician is named by hand.
  private readonly suggestions = httpResource<VoiceSuggestions>(
    () => `/api/sessions/${this.id()}/voices`,
  );
  private readonly voiceSuggestions = computed(() => this.suggestions.value()?.suggestions ?? {});

  protected readonly conversations = computed(() => this.data.value()?.items ?? []);
  protected readonly start = computed(() => this.conversations()[0]?.start ?? null);
  protected readonly empty = computed(
    () => !this.conversations().length && !this.data.isLoading(),
  );

  protected readonly turns = computed(() =>
    this.conversations()
      .flatMap((c) => c.moments)
      .flatMap((m) => m.primary),
  );

  // A turn is final once diarized or human-corrected; 'live'/'transcribed' are
  // provisional and will be replaced by the refine pass.
  protected readonly finalizing = computed(() =>
    this.turns().some((t) => t.tier === 'live' || t.tier === 'transcribed'),
  );
  protected readonly ready = computed(() => this.turns().length > 0 && !this.finalizing());

  // The distinct voices, biggest first, each with the name most of its turns carry,
  // a representative sample turn to identify it by, and a voiceprint-based suggestion.
  protected readonly voices = computed<Voice[]>(() => {
    interface Acc { counts: Map<string, number>; turns: Transcript[] }
    const byCluster = new Map<string, Acc>();
    for (const t of this.turns()) {
      if (!t.cluster) continue;
      let e = byCluster.get(t.cluster);
      if (!e) {
        e = { counts: new Map(), turns: [] };
        byCluster.set(t.cluster, e);
      }
      e.turns.push(t);
      if (t.speakerConfirmed && t.speaker) {
        e.counts.set(t.speaker, (e.counts.get(t.speaker) ?? 0) + 1);
      }
    }
    const suggested = this.voiceSuggestions();
    return [...byCluster.entries()]
      .sort((a, b) => b[1].turns.length - a[1].turns.length)
      .map(([cluster, e]) => {
        let name: string | null = null;
        let best = 0;
        for (const [n, k] of e.counts) {
          if (k > best) {
            best = k;
            name = n;
          }
        }
        // The median-length turn is a typical sample — avoids the longest (often a
        // mis-clustered outlier, e.g. a monologue) and the trivial short ones.
        const byLen = [...e.turns].sort((a, b) => a.text.length - b.text.length);
        const sample = byLen[Math.floor(byLen.length / 2)];
        return {
          cluster,
          // Only named when most of the voice's turns carry that name — a few stray
          // reassigned turns must not relabel the whole voice.
          name: best > e.turns.length / 2 ? name : null,
          turns: e.turns.length,
          sample: sample?.text ?? '',
          sampleUrl: sample?.audioUrl ?? '',
          suggested: suggested[cluster] ?? null,
        };
      });
  });

  // Stable "Voice N" numbering by the cluster's position in the voices list.
  private readonly clusterNo = computed(() => {
    const idx = new Map<string, number>();
    this.voices().forEach((v, i) => idx.set(v.cluster, i + 1));
    return idx;
  });

  protected voiceLabel(t: Transcript): string {
    if (t.speakerConfirmed && t.speaker) return t.speaker;
    return t.cluster ? `Voice ${this.clusterNo().get(t.cluster) ?? '?'}` : 'unknown';
  }

  // Consecutive same-speaker turns coalesced into one paragraph (so a fixed split reads
  // as one, and merge is free — just relabel neighbours). The turns stay separate.
  protected readonly runs = computed<Run[]>(() => {
    const out: Run[] = [];
    for (const t of this.turns()) {
      const speaker = this.voiceLabel(t);
      const last = out[out.length - 1];
      if (last?.speaker === speaker) {
        (last.turns as Transcript[]).push(t);
      } else {
        out.push({
          key: t.id,
          speaker,
          confirmed: t.speakerConfirmed,
          start: t.start,
          turns: [t],
        });
      }
    }
    return out;
  });

  // The people you can assign a selection to: everyone confirmed on *any* turn of this
  // session, in first-seen order — so a third person added on a single turn (whose
  // cluster is still majority someone else) is reusable with one tap on the next
  // marking. Cluster-majority naming drives the cast above, not this. (The full
  // known-names list still feeds the name autocomplete, for introducing someone new.)
  protected readonly palette = computed(() => {
    const names: string[] = [];
    for (const t of this.turns()) {
      if (t.speakerConfirmed && t.speaker && !names.includes(t.speaker)) {
        names.push(t.speaker);
      }
    }
    return names;
  });

  // A distinct, readable-on-dark colour per speaker so the conversation scans at a
  // glance (who's talking) instead of reading as one wall of text. Assigned by order
  // of appearance; stable within a session.
  private readonly PALETTE = [
    '#8ab4f8', '#fbbc04', '#81c995', '#f28b82', '#c58af9', '#78d9ec', '#ff8bcb',
  ];
  private readonly speakerColors = computed(() => {
    const colour = new Map<string, string>();
    for (const run of this.runs()) {
      if (!colour.has(run.speaker)) {
        colour.set(run.speaker, this.PALETTE[colour.size % this.PALETTE.length]);
      }
    }
    return colour;
  });
  protected colourFor(speaker: string): string {
    return this.speakerColors().get(speaker) ?? 'var(--mat-sys-on-surface-variant)';
  }

  // What's currently playing — a key like 'run:42' / 'voice:SPEAKER_00' / 'span'. One
  // shared <audio> and one toggle: tap the same thing to pause it (keeping its place) and
  // tap again to resume; starting anything else stops whatever was playing. Played
  // straight from the tap (a real user gesture, so it reliably plays), no native widget.
  protected readonly playing = signal<string | null>(null);
  private readonly audio = new Audio();
  private loadedUrl = '';

  /** Stop the shared clip on teardown — otherwise leaving the session mid-playback
   * leaves the <audio> playing (and holding its network src). */
  ngOnDestroy(): void {
    this.audio.pause();
    this.audio.src = '';
    this.playing.set(null);
  }

  private play(key: string, url: string): void {
    if (this.playing() === key) {
      this.audio.pause(); // pause in place — resumes from here on the next tap
      this.playing.set(null);
      return;
    }
    if (this.loadedUrl !== url) {
      // a different clip — load it from the start (replacing whatever was loaded)
      this.loadedUrl = url;
      this.audio.onended = () => this.playing.set(null);
      this.audio.src = url;
    }
    void this.audio.play(); // resumes a paused clip, or starts a fresh one
    this.playing.set(key);
  }

  /** Play / stop a whole speaker bubble — one continuous clip across all its turns
   * (their full span), not just the first. */
  protected togglePlay(run: Run): void {
    const turns = run.turns;
    const url = `/api/audio-span?from_id=${turns[0].id}&to_id=${turns[turns.length - 1].id}`;
    this.play(`run:${run.key}`, url);
  }

  /** Play / stop a voice's sample clip from the cast. */
  protected toggleSample(cluster: string, url: string): void {
    this.play(`voice:${cluster}`, url);
  }

  // The turn you've tapped (null = none). A single tap selects the whole turn; a
  // drag-select of part of the text opens the assign-span bar (`span`) instead.
  protected readonly selected = signal<number | null>(null);

  // A text range selected across turns (drag-select), in turn-id + char-offset terms,
  // plus its text — drives the "assign to" bar that moves a phrase to the right speaker
  // without retyping. Null when nothing is range-selected.
  protected readonly span = signal<SpanSel | null>(null);
  protected readonly spanText = signal('');

  /** Tap a turn to select it; tap it again to deselect. A range selection wins — a tap
   * that's really the tail of a drag must not also select the whole turn. */
  protected selectTurn(id: number): void {
    if (this.span()) return;
    this.selected.update((cur) => (cur === id ? null : id));
  }

  /** On mouse/touch up over the transcript: open the assign-span bar for a non-empty
   * text selection, else clear it (a plain tap falls through to selectTurn). */
  protected onSelect(): void {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed || sel.rangeCount === 0) {
      this.span.set(null);
      return;
    }
    const span = this.selectionToSpan(sel.getRangeAt(0));
    this.span.set(span);
    this.spanText.set(span ? sel.toString().trim() : '');
  }

  /** Map a DOM range to a {startTurn,startChar,endTurn,endChar} span, or null if either
   * endpoint isn't inside a turn (e.g. it drifted onto a speaker name). Shared with the
   * timeline via `resolveSelection`. */
  protected selectionToSpan(range: Range): SpanSel | null {
    return resolveSelection(range)?.span ?? null;
  }

  // True while an assign request is in flight — so an impatient double-tap can't fire
  // the same split several times (which the server would race into duplicate pieces).
  protected readonly assigning = signal(false);

  /** Assign the selected span to `name`: the backend splits at the edges and relabels —
   * no retyping, and the moved phrase keeps its word-snapped audio. */
  protected assignSelectedSpan(name: string): void {
    const span = this.span();
    const who = name.trim();
    if (!span || !who || this.assigning()) return;
    this.assigning.set(true);
    this.api.assignSpan(this.id(), { ...span, name: who }).subscribe({
      next: () => {
        this.clearSpan();
        this.data.reload();
        this.speakerNames.reload(); // a new name autocompletes immediately elsewhere
        this.assigning.set(false);
      },
      error: () => {
        this.assigning.set(false);
        this.fail();
      },
    });
  }

  /** Dismiss the assign-span bar and drop the native selection. */
  protected clearSpan(): void {
    window.getSelection()?.removeAllRanges();
    this.span.set(null);
  }

  /** Assign the tapped turn to a person — `name` may be a brand-new one. */
  protected assignTurn(name: string): void {
    const id = this.selected();
    const who = name.trim();
    if (id === null || !who || this.assigning()) return;
    this.assigning.set(true);
    this.api.setTurnSpeaker(id, who).subscribe({
      next: () => {
        this.data.reload();
        this.speakerNames.reload(); // a new name autocompletes immediately elsewhere
        this.selected.set(null);
        this.assigning.set(false);
      },
      error: () => {
        this.assigning.set(false);
        this.fail();
      },
    });
  }

  protected nameVoice(cluster: string, name: string): void {
    this.api.nameSessionVoice(this.id(), cluster, name.trim()).subscribe({
      next: () => {
        this.data.reload();
        this.speakerNames.reload(); // a new name suggests immediately elsewhere
      },
      error: () => this.fail(),
    });
  }

  // Bumped after each boundary nudge so the <audio> re-fetches the (now re-sliced) clip
  // — same turn id, different span, so the URL must change to dodge the cache.
  private nudgeVersion = 0;

  /** Play / stop the tapped turn — hear who said it before assigning. */
  protected playSelected(): void {
    const id = this.selected();
    if (id !== null) this.play('turn', `/api/audio/${id}?v=${this.nudgeVersion}`);
  }

  /** Hand-tune the selected turn's boundary by ear: move an edge, then replay so you
   * hear the trimmed clip. Whisper gives the first cut; this nudges it ±0.1s. */
  protected nudgeTurn(edge: 'start' | 'end', delta: number): void {
    const id = this.selected();
    if (id === null) return;
    this.api.nudgeTurn(id, edge, delta).subscribe({
      next: () => {
        this.nudgeVersion++;
        this.data.reload();
        this.playing.set(null); // don't toggle-pause — start the new clip fresh
        this.playSelected();
      },
      error: () => this.fail(),
    });
  }

  // Which turn is being text-edited (null = none) — fixing the *words*, a separate
  // intent from assigning *who* said them. Opened on the tapped turn.
  protected readonly editing = signal<number | null>(null);
  protected readonly editingText = computed(
    () => this.turns().find((t) => t.id === this.editing())?.text ?? '',
  );

  protected editText(): void {
    const id = this.selected();
    if (id !== null) this.editing.set(id);
  }

  /** Save a text correction. The backend keeps the turn's speaker (it isn't passed). */
  protected saveEdit(text: string): void {
    const id = this.editing();
    this.editing.set(null);
    this.selected.set(null);
    if (id === null || !text.trim()) return;
    this.api
      .correct(id, text.trim())
      .subscribe({ next: () => this.data.reload(), error: () => this.fail() });
  }

  protected cancelEdit(): void {
    this.editing.set(null);
  }

  private fail(): void {
    this.snack.open('Could not save — try again', 'OK', { duration: 4000 });
  }

  protected readonly day = dayLabel;
  protected readonly time = timeOfDay;
  protected readonly clock = timeOfDaySeconds;
}
