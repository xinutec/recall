import { durationUntil, formatConfidence, formatDuration, timeOfDaySeconds } from './format';
import { Transcript } from './models';

function turn(start: string, end: string): Transcript {
  return {
    id: 1,
    start,
    end,
    text: '',
    language: null,
    speaker: null,
    speakerConfirmed: false,
    speakerConfidence: null,
    confidence: null,
    loudness: null,
    model: 'live',
    tier: 'live',
    hidden: null,
    audioUrl: '',
    source: null,
    cluster: null,
  };
}

describe('format', () => {
  it('renders confidence as a rounded percentage', () => {
    expect(formatConfidence(0.873)).toBe('87%');
    expect(formatConfidence(null)).toBe('');
  });

  it('renders positive durations and nothing for degenerate spans', () => {
    expect(formatDuration(turn('2026-06-13T10:00:00Z', '2026-06-13T10:00:03.5Z'))).toBe('3.5s');
    expect(formatDuration(turn('2026-06-13T10:00:00Z', '2026-06-13T10:00:00Z'))).toBe('');
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
