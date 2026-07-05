/** Presentation helpers for A/B comparison runs — pure, so they're unit-tested and
 * shared by the list and the run view. */

export type VerdictTone = 'better' | 'worse' | 'tie' | 'pending' | 'unknown';

export interface Verdict {
  readonly label: string;
  readonly tone: VerdictTone;
}

/** A WER (0..1) as a percentage, or an em-dash when it's unknown (no corrections). */
export function werPct(value: number | null): string {
  return value === null ? '—' : `${(value * 100).toFixed(1)}%`;
}

/** The headline verdict for a run: its status while not done, else whether B (the new
 * model) beat A on mean WER — or that there was no ground truth to score against. */
export function verdictOf(run: {
  readonly status: string;
  readonly meanWerA: number | null;
  readonly meanWerB: number | null;
}): Verdict {
  if (run.status === 'queued') return { label: 'Queued', tone: 'pending' };
  if (run.status === 'running') return { label: 'Running…', tone: 'pending' };
  if (run.status === 'error') return { label: 'Failed', tone: 'worse' };
  const { meanWerA: a, meanWerB: b } = run;
  if (a === null || b === null) return { label: 'No WER', tone: 'unknown' };
  if (b < a) return { label: 'B better', tone: 'better' };
  if (b > a) return { label: 'A better', tone: 'worse' };
  return { label: 'Tie', tone: 'tie' };
}
