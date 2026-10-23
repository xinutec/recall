/** Presentation helpers for transcript fields. Pure, unit-testable. */

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

/** A day short enough for a phone's top bar beside a time: "Thu 15 Jan 2026". */
export function shortDay(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleDateString(undefined, {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
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

/** The next `hh:mm` (local) after `from`: today, or tomorrow once today's has
 *  passed, so "until 01:00" late at night means the coming night. `null` for
 *  text that is not a time. */
export function nextTimeOfDay(hhmm: string, from: Date): Date | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(hhmm);
  if (!match) {
    return null;
  }
  const [h, m] = [Number(match[1]), Number(match[2])];
  if (h > 23 || m > 59) {
    return null;
  }
  const at = new Date(from);
  at.setHours(h, m, 0, 0);
  if (at <= from) {
    at.setDate(at.getDate() + 1);
  }
  return at;
}
