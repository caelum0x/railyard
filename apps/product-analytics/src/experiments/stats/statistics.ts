// Sufficient statistics for experiment metrics. Ported from PostHog
// products/experiments/stats/shared/{statistics,utils}.py (MIT, commit
// 645e1a78140757ea1bb9ddeb0ff9d3915c60b6f6): the same validation, means,
// variances and the delta method for ratios.

export class StatisticError extends Error {}

export interface MeanStat {
  kind: "mean";
  n: number;
  sum: number;
  sumSquares: number;
}

export interface ProportionStat {
  kind: "proportion";
  n: number;
  sum: number;
}

export interface RatioStat {
  kind: "ratio";
  n: number;
  m: MeanStat | ProportionStat;
  d: MeanStat | ProportionStat;
  mdSumOfProducts: number;
}

export type Stat = MeanStat | ProportionStat | RatioStat;

export function checkStat(s: Stat): void {
  if (!(s.n > 0)) throw new StatisticError("Sample size must be positive");
  if (s.kind === "mean") {
    if (s.n === 1 && s.sumSquares < s.sum ** 2) throw new StatisticError("sum_squares must be >= sum^2 for single observation");
    if (s.n > 1 && s.sumSquares < s.sum ** 2 / s.n) throw new StatisticError("sum_squares incompatible with sum and n");
  } else if (s.kind === "proportion") {
    if (s.sum < 0) throw new StatisticError("Number of successes cannot be negative");
    if (s.sum > s.n) throw new StatisticError("Number of successes cannot exceed sample size");
  } else {
    checkStat(s.m);
    checkStat(s.d);
    if (s.m.n !== s.n || s.d.n !== s.n) throw new StatisticError("All statistics must have same sample size");
    if (Math.abs(meanOf(s.d)) < 1e-10) throw new StatisticError("Denominator mean cannot be zero for ratio calculation");
  }
}

export function meanOf(s: Stat): number {
  if (s.kind === "ratio") return s.m.sum / s.d.sum;
  return s.sum / s.n;
}

export function varianceOf(s: Stat): number {
  if (s.kind === "mean") return s.n === 1 ? 0 : (s.sumSquares - s.sum ** 2 / s.n) / (s.n - 1);
  if (s.kind === "proportion") {
    const p = s.sum / s.n;
    return p * (1 - p);
  }
  const mMean = meanOf(s.m);
  const dMean = meanOf(s.d);
  const cov = s.n === 1 ? 0 : (s.mdSumOfProducts - (s.m.sum * s.d.sum) / s.n) / (s.n - 1);
  return varianceOf(s.m) / dMean ** 2 + (mMean ** 2 * varianceOf(s.d)) / dMean ** 4 - (2 * mMean * cov) / dMean ** 3;
}

/** shared/utils.py validate_statistic_inputs, as used by the Bayesian path. */
export function validationErrors(s: Stat): string[] {
  const errors: string[] = [];
  const mean = meanOf(s);
  const variance = varianceOf(s);
  if (!(s.n > 0)) errors.push("Sample size must be positive");
  if (!Number.isFinite(mean)) errors.push("Mean must be finite");
  if (!Number.isFinite(variance) || variance < 0) errors.push("Variance must be finite and non-negative");
  if (s.kind === "mean") {
    if (s.n > 1 && s.sumSquares < s.sum ** 2 / s.n - 1e-10) errors.push("sum_squares violates mathematical constraint");
    if (Math.abs(s.sum) > 1e15) errors.push("Sum value extremely large - potential overflow risk");
    if (s.sumSquares > 1e30) errors.push("Sum of squares extremely large - potential overflow risk");
  } else if (s.kind === "proportion") {
    const p = s.sum / s.n;
    if (p < 1e-10 || p > 1 - 1e-10) errors.push("Proportion extremely close to 0 or 1 - statistical tests may be unreliable");
  }
  return errors;
}

export function sameKind(t: Stat, c: Stat): void {
  if (t.kind !== c.kind) throw new StatisticError("Treatment and control statistics must be the same type");
}
