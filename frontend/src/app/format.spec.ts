import { durationUntil, nextTimeOfDay, shortDay, timeOfDaySeconds } from './format';

describe('format', () => {
  it('shortDay is short enough for a bar beside a time, and keeps the year', () => {
    const day = shortDay('2026-01-15T12:00:00Z');
    expect(day).toContain('2026');
    expect(day.length).toBeLessThanOrEqual(17);
    expect(shortDay('not a date')).toBe('not a date');
  });

  it('renders a to-the-second wall clock, and passes through junk', () => {
    expect(timeOfDaySeconds('2026-01-15T10:41:51+00:00')).toMatch(/^\d{2}:\d{2}:\d{2}/);
    expect(timeOfDaySeconds('not-a-date')).toBe('not-a-date');
  });

  it('renders time remaining as hours/minutes', () => {
    const from = Date.parse('2026-07-03T17:07:00Z');
    expect(durationUntil('2026-07-03T22:30:00Z', from)).toBe('5h 23m');
    expect(durationUntil('2026-07-03T17:30:00Z', from)).toBe('23m');
    expect(durationUntil('2026-07-03T18:07:00Z', from)).toBe('1h 0m');
    expect(durationUntil('2026-07-03T17:07:20Z', from)).toBe('now'); // <30s rounds to 0
    expect(durationUntil('2026-07-03T16:00:00Z', from)).toBe('now'); // past → never negative
    expect(durationUntil('not-a-date', from)).toBe('');
  });
});

describe('nextTimeOfDay', () => {
  // Local times throughout: the picker speaks the viewer's clock.
  const from = new Date(2026, 9, 23, 22, 30);

  it('is later today when the time is still ahead', () => {
    expect(nextTimeOfDay('23:15', from)).toEqual(new Date(2026, 9, 23, 23, 15));
  });

  it('is tomorrow once the time has passed, or is now', () => {
    expect(nextTimeOfDay('01:00', from)).toEqual(new Date(2026, 9, 24, 1, 0));
    expect(nextTimeOfDay('22:30', from)).toEqual(new Date(2026, 9, 24, 22, 30));
  });

  it('refuses what is not a time', () => {
    for (const text of ['', '7', '24:00', '12:60', 'noon']) {
      expect(nextTimeOfDay(text, from)).toBeNull();
    }
  });
});
