// Distribution functions for the experiment statistics, from jStat 1.9.6
// (MIT, vendored unmodified in ../vendor/jstat). The upstream Python uses
// scipy; test/a3/stats.test.ts checks these against scipy's values.

import { createRequire } from "node:module";

interface JStat {
  normal: { cdf(x: number, mean: number, sd: number): number; inv(p: number, mean: number, sd: number): number };
  studentt: { cdf(x: number, dof: number): number; inv(p: number, dof: number): number };
  chisquare: { cdf(x: number, dof: number): number };
  lowRegGamma(a: number, x: number): number;
  gammaln(x: number): number;
}

const jStat = (createRequire(import.meta.url)("../vendor/jstat/jstat.cjs") as { jStat: JStat }).jStat;

export const normalCdf = (x: number, mean = 0, sd = 1): number => jStat.normal.cdf(x, mean, sd);

/** Survival function 1 - cdf, computed on the far side for precision in the upper tail. */
export const normalSf = (x: number, mean = 0, sd = 1): number => jStat.normal.cdf(2 * mean - x, mean, sd);

export const normalPpf = (p: number, mean = 0, sd = 1): number => jStat.normal.inv(p, mean, sd);

export const studentTCdf = (x: number, dof: number): number => jStat.studentt.cdf(x, dof);

export const studentTPpf = (p: number, dof: number): number => jStat.studentt.inv(p, dof);

/**
 * Upper tail of the chi-square distribution, Q(k/2, x/2). jStat's chisquare.cdf
 * is P; 1 - P loses every digit below 1e-16, so the upper tail comes from the
 * continued fraction for the regularised upper incomplete gamma (Numerical
 * Recipes gcf, the same method jStat uses for the lower tail's other branch).
 */
export function chiSquareSf(x: number, dof: number): number {
  if (x <= 0) return 1;
  const a = dof / 2;
  const z = x / 2;
  if (z < a + 1) return 1 - jStat.lowRegGamma(a, z);
  const FPMIN = 1e-300;
  let b = z + 1 - a;
  let c = 1 / FPMIN;
  let d = 1 / b;
  let h = d;
  for (let i = 1; i <= 1000; i += 1) {
    const an = -i * (i - a);
    b += 2;
    d = an * d + b;
    if (Math.abs(d) < FPMIN) d = FPMIN;
    c = b + an / c;
    if (Math.abs(c) < FPMIN) c = FPMIN;
    d = 1 / d;
    const del = d * c;
    h *= del;
    if (Math.abs(del - 1) < 1e-15) break;
  }
  return Math.exp(-z + a * Math.log(z) - jStat.gammaln(a)) * h;
}
