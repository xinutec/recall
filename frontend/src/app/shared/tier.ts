import { Tier } from '../models';

/** No speaker separation yet: shown grey, still editable. */
export function undiarized(tier: Tier): boolean {
  return tier === 'live' || tier === 'transcribed';
}
