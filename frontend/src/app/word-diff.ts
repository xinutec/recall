/** A word-level diff between two transcriptions, for highlighting *what changed*
 * between model A and model B (or against the ground truth). Pure and framework-free
 * so it's unit-tested directly. Matching ignores case and punctuation ("Mask." ==
 * "mask"), so only real wording differences are flagged, not spacing/casing noise. */

export interface DiffToken {
  readonly text: string;
  readonly changed: boolean;
}

export interface WordDiff {
  readonly a: readonly DiffToken[];
  readonly b: readonly DiffToken[];
}

function norm(token: string): string {
  return token.toLowerCase().replace(/[^a-z0-9]/g, '');
}

/** Tokens of `a` and `b`, each flagged `changed` where it isn't part of the longest
 * common subsequence — i.e. words one side has that the other doesn't. */
export function wordDiff(a: string, b: string): WordDiff {
  const at = a.split(/\s+/).filter(Boolean);
  const bt = b.split(/\s+/).filter(Boolean);
  const n = at.length;
  const m = bt.length;

  // dp[i][j] = LCS length of at[i:] and bt[j:]; backtrack from (0,0).
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i][j] =
        norm(at[i]) === norm(bt[j]) ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
    }
  }

  const aOut: DiffToken[] = [];
  const bOut: DiffToken[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (norm(at[i]) === norm(bt[j])) {
      aOut.push({ text: at[i], changed: false });
      bOut.push({ text: bt[j], changed: false });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) {
      aOut.push({ text: at[i], changed: true });
      i++;
    } else {
      bOut.push({ text: bt[j], changed: true });
      j++;
    }
  }
  while (i < n) {
    aOut.push({ text: at[i], changed: true });
    i++;
  }
  while (j < m) {
    bOut.push({ text: bt[j], changed: true });
    j++;
  }
  return { a: aOut, b: bOut };
}
