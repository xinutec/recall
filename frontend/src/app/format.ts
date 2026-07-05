/** Presentation helpers for transcript fields. Pure, unit-testable. */

import { Transcript } from './models';

export function formatClock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleString(undefined, {
    year: 'numeric',
    month: 'short',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

export function formatDuration(t: Transcript): string {
  const seconds = (new Date(t.end).getTime() - new Date(t.start).getTime()) / 1000;
  if (!Number.isFinite(seconds) || seconds <= 0) {
    return '';
  }
  return `${seconds.toFixed(1)}s`;
}

export function dayLabel(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleDateString(undefined, {
    weekday: 'long',
    day: '2-digit',
    month: 'long',
    year: 'numeric',
  });
}

/** Stable local-day key (YYYY-MM-DD in the viewer's timezone) for grouping. */
export function dayKey(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  const y = d.getFullYear();
  const m = `${d.getMonth() + 1}`.padStart(2, '0');
  const day = `${d.getDate()}`.padStart(2, '0');
  return `${y}-${m}-${day}`;
}

export function timeOfDay(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
}

/** Time remaining from `from` until `iso`, as "5h 23m" / "23m" / "now".
 *  Whole minutes, never negative (a past deadline reads "now"). Empty on a
 *  bad date. `from` is injectable for tests. */
export function durationUntil(iso: string, from: number = Date.now()): string {
  const target = new Date(iso).getTime();
  if (Number.isNaN(target)) {
    return '';
  }
  const mins = Math.max(0, Math.round((target - from) / 60_000));
  if (mins === 0) {
    return 'now';
  }
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  return h > 0 ? `${h}h ${m}m` : `${m}m`;
}

/** Local wall-clock to the second (HH:MM:SS) — for pinning a turn to the exact moment. */
export function timeOfDaySeconds(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

export function formatConfidence(confidence: number | null): string {
  if (confidence === null) {
    return '';
  }
  return `${Math.round(confidence * 100)}%`;
}
