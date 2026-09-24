import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  OnDestroy,
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

import { ConversationPage, SpeakerNames, Transcript } from '../models';
import { RecallApi } from '../recall-api';
import { Player } from '../shared/player';
import { Turns } from '../shared/turns';
import { dayLabel, timeOfDay } from '../format';

/** A diarization voice: its cluster, the name most of its turns carry, and a
 * sample to recognise it by. */
interface Voice {
  readonly cluster: string;
  readonly name: string | null;
  readonly turns: number;
  readonly sampleUrl: string;
}

/** One recording: the timeline's view of its turns, plus naming its voices. */
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
    Turns,
  ],
  templateUrl: './session.html',
  styleUrl: './session.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class Session implements OnDestroy {
  readonly id = input.required<string>();

  private readonly snack = inject(MatSnackBar);
  private readonly api = inject(RecallApi);
  protected readonly player = inject(Player);

  // A recording is bounded, so one fetch holds all of it.
  protected readonly data = httpResource<ConversationPage>(() => {
    const params = new URLSearchParams({ source: this.id(), limit: '5000' });
    return `/api/conversations?${params.toString()}`;
  });
  private readonly speakerNames = httpResource<SpeakerNames>(() => '/api/speakers');
  protected readonly knownNames = computed(() => this.speakerNames.value()?.names ?? []);

  protected readonly moments = computed(() =>
    (this.data.value()?.items ?? []).flatMap((c) => c.moments),
  );
  protected readonly turns = computed(() => this.moments().flatMap((m) => m.primary));
  protected readonly start = computed(() => this.turns()[0]?.start ?? null);
  protected readonly empty = computed(() => !this.turns().length && !this.data.isLoading());

  protected readonly finalizing = computed(() =>
    this.turns().some((t) => t.tier === 'live' || t.tier === 'transcribed'),
  );

  /** Voices, biggest first. Named only when most of its turns carry the name. */
  protected readonly voices = computed<Voice[]>(() => {
    const byCluster = new Map<string, { counts: Map<string, number>; turns: Transcript[] }>();
    for (const t of this.turns()) {
      if (!t.cluster) continue;
      const e = byCluster.get(t.cluster) ?? { counts: new Map<string, number>(), turns: [] as Transcript[] };
      byCluster.set(t.cluster, e);
      e.turns.push(t);
      if (t.speakerConfirmed && t.speaker) {
        e.counts.set(t.speaker, (e.counts.get(t.speaker) ?? 0) + 1);
      }
    }
    return [...byCluster.entries()]
      .sort((a, b) => b[1].turns.length - a[1].turns.length)
      .map(([cluster, e]) => {
        const [name, best] = [...e.counts].reduce<[string | null, number]>(
          (acc, cur) => (cur[1] > acc[1] ? cur : acc),
          [null, 0],
        );
        // The median-length turn: the longest is often a mis-clustered outlier.
        const byLen = [...e.turns].sort((a, b) => a.text.length - b.text.length);
        const sample = byLen[Math.floor(byLen.length / 2)];
        return {
          cluster,
          name: best > e.turns.length / 2 ? name : null,
          turns: e.turns.length,
          sampleUrl: sample?.audioUrl ?? '',
        };
      });
  });

  protected readonly voiceNames = computed(
    () => new Map(this.voices().map((v, i) => [v.cluster, `Voice ${i + 1}`])),
  );

  protected toggleSample(v: Voice): void {
    this.player.toggle(`voice:${v.cluster}`, v.sampleUrl);
  }

  protected nameVoice(cluster: string, name: string): void {
    this.api.nameSessionVoice(this.id(), cluster, name.trim()).subscribe({
      next: () => this.reload(),
      error: () => this.snack.open('Could not save, try again', 'OK', { duration: 4000 }),
    });
  }

  protected reload(): void {
    this.data.reload();
    this.speakerNames.reload();
  }

  ngOnDestroy(): void {
    if (this.player.playing()?.startsWith('voice:')) this.player.stop();
  }

  protected readonly day = dayLabel;
  protected readonly time = timeOfDay;
}
