import { describe, expect, it } from 'vitest';

import { verdictOf, werPct } from './ab-compare-format';

describe('werPct', () => {
  it('formats a fraction as a percentage', () => {
    expect(werPct(0.253)).toBe('25.3%');
    expect(werPct(0)).toBe('0.0%');
  });

  it('shows an em-dash when unknown', () => {
    expect(werPct(null)).toBe('—');
  });
});

describe('verdictOf', () => {
  it('reflects in-progress status before a result exists', () => {
    expect(verdictOf({ status: 'queued', meanWerA: null, meanWerB: null }).tone).toBe('pending');
    expect(verdictOf({ status: 'running', meanWerA: null, meanWerB: null }).tone).toBe('pending');
    expect(verdictOf({ status: 'error', meanWerA: null, meanWerB: null }).tone).toBe('worse');
  });

  it('calls B better when it has the lower WER', () => {
    const v = verdictOf({ status: 'done', meanWerA: 0.21, meanWerB: 0.18 });
    expect(v).toEqual({ label: 'B better', tone: 'better' });
  });

  it('calls A better when B regressed', () => {
    const v = verdictOf({ status: 'done', meanWerA: 0.21, meanWerB: 0.25 });
    expect(v).toEqual({ label: 'A better', tone: 'worse' });
  });

  it('reports no WER when there was no ground truth', () => {
    expect(verdictOf({ status: 'done', meanWerA: null, meanWerB: null }).tone).toBe('unknown');
  });
});
