import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  inject,
  input,
  signal,
} from '@angular/core';
import { httpResource } from '@angular/common/http';
import { RouterLink } from '@angular/router';
import { MatButtonModule } from '@angular/material/button';
import { MatCardModule } from '@angular/material/card';
import { MatIconModule } from '@angular/material/icon';
import { MatProgressBarModule } from '@angular/material/progress-bar';

import { AbCompareRun, AbCompareScore, AbCompareSegmentDiff } from '../models';
import { DiffToken, wordDiff } from '../word-diff';
import { verdictOf, werPct } from '../ab-compare-format';

const POLL_MS = 4_000;

/** One scored span turned into an evidence card: the two models' words highlighted
 * against the ground truth (so each model's errors are visible), each WER, the gap
 * between them (drives the sort), and which model won. */
interface EvidenceCard {
  readonly score: AbCompareScore;
  readonly aTokens: readonly DiffToken[];
  readonly bTokens: readonly DiffToken[];
  readonly gap: number;
  readonly winner: 'A' | 'B' | 'tie';
}

interface SegmentCard {
  readonly diff: AbCompareSegmentDiff;
  readonly aTokens: readonly DiffToken[];
  readonly bTokens: readonly DiffToken[];
}

/** Build the evidence cards, biggest A↔B WER gap first, each model's words diffed
 * against the ground truth so its errors are highlighted. Pure, so it's unit-tested. */
export function buildEvidenceCards(scores: readonly AbCompareScore[]): EvidenceCard[] {
  return scores
    .map((score) => ({
      score,
      aTokens: wordDiff(score.truth, score.textA).b,
      bTokens: wordDiff(score.truth, score.textB).b,
      gap: Math.abs(score.werA - score.werB),
      winner: scoreWinner(score),
    }))
    .sort((x, y) => y.gap - x.gap);
}

/** The whole-segment diffs that actually differ, A vs B highlighted. */
export function buildSegmentCards(diffs: readonly AbCompareSegmentDiff[]): SegmentCard[] {
  return diffs
    .filter((d) => d.changed)
    .map((diff) => {
      const wd = wordDiff(diff.textA, diff.textB);
      return { diff, aTokens: wd.a, bTokens: wd.b };
    });
}

function scoreWinner(score: AbCompareScore): 'A' | 'B' | 'tie' {
  if (score.werB < score.werA) return 'B';
  if (score.werB > score.werA) return 'A';
  return 'tie';
}

@Component({
  selector: 'app-compare-run',
  imports: [RouterLink, MatButtonModule, MatCardModule, MatIconModule, MatProgressBarModule],
  templateUrl: './compare-run.html',
  styleUrl: './compare-run.scss',
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export class CompareRun {
  readonly id = input.required<string>();

  protected readonly data = httpResource<AbCompareRun>(() => `/api/ab-compare/${this.id()}`);
  protected readonly summary = computed(() => this.data.value()?.summary ?? null);
  protected readonly verdict = computed(() => {
    const s = this.summary();
    return s ? verdictOf(s) : null;
  });

  // The cards, biggest A↔B disagreement first — the regressions and wins that matter
  // float to the top instead of being buried in a flat list.
  protected readonly cards = computed<EvidenceCard[]>(() =>
    buildEvidenceCards(this.data.value()?.scores ?? []),
  );

  protected readonly segments = computed<SegmentCard[]>(() =>
    buildSegmentCards(this.data.value()?.segmentDiffs ?? []),
  );

  // Per-card "play with surrounding context" toggle (default: the exact, tight span).
  private readonly context = signal<ReadonlySet<number>>(new Set());

  constructor() {
    // Poll while the run is still working; cleared on destroy so a closed run
    // page doesn't keep reloading forever.
    const poller = setInterval(() => {
      const s = this.summary();
      if (s && (s.status === 'queued' || s.status === 'running')) {
        this.data.reload();
      }
    }, POLL_MS);
    inject(DestroyRef).onDestroy(() => clearInterval(poller));
  }

  protected audioSrc(score: AbCompareScore): string {
    return this.context().has(score.correctionId)
      ? `${score.audioUrl}?context=true`
      : score.audioUrl;
  }

  protected inContext(score: AbCompareScore): boolean {
    return this.context().has(score.correctionId);
  }

  protected toggleContext(score: AbCompareScore): void {
    const next = new Set(this.context());
    if (next.has(score.correctionId)) {
      next.delete(score.correctionId);
    } else {
      next.add(score.correctionId);
    }
    this.context.set(next);
  }

  protected readonly pct = werPct;
}
