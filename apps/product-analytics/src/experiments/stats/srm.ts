// Sample-ratio mismatch: a chi-square goodness-of-fit test of the exposure
// counts against the planned weights (the same test as scipy.stats.chisquare,
// which the upstream experiment tooling uses). A p-value under 0.001 means the
// split is not the one planned, so the results cannot be trusted as they are.

import { chiSquareSf } from "./distributions.ts";

export const SRM_P_THRESHOLD = 0.001;

export interface SrmResult {
  chi2: number;
  pValue: number;
  mismatch: boolean;
  expected: number[];
}

export function sampleRatioCheck(counts: number[], weights: number[]): SrmResult {
  if (counts.length !== weights.length || counts.length < 2) throw new Error("counts and weights must have the same length (2 or more)");
  const total = counts.reduce((a, b) => a + b, 0);
  const wsum = weights.reduce((a, b) => a + b, 0);
  const expected = weights.map((w) => (w / wsum) * total);
  if (total === 0 || expected.some((e) => e <= 0)) return { chi2: 0, pValue: 1, mismatch: false, expected };
  const chi2 = counts.reduce((acc, o, i) => acc + (o - expected[i]) ** 2 / expected[i], 0);
  const pValue = chiSquareSf(chi2, counts.length - 1);
  return { chi2, pValue, mismatch: pValue < SRM_P_THRESHOLD, expected };
}
