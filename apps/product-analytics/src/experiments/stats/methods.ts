// Frequentist and Bayesian tests for experiments. Ported line by line from
// PostHog products/experiments/stats/{frequentist,bayesian}/*.py (MIT, commit
// 645e1a78140757ea1bb9ddeb0ff9d3915c60b6f6); checked against the upstream
// code within 1e-6 and against GrowthBook packages/stats test vectors (MIT)
// in test/a3/stats.test.ts.

import { normalCdf, normalPpf, normalSf, studentTCdf, studentTPpf } from "./distributions.ts";
import { checkStat, meanOf, sameKind, StatisticError, validationErrors, varianceOf, type Stat } from "./statistics.ts";

export { StatisticError, type Stat } from "./statistics.ts";

export type Difference = "relative" | "absolute";

export interface FrequentistOptions {
  difference: Difference;
  alpha: number;
  /** Sequential testing (always-valid p-values) with this tuning parameter, or null for a fixed-horizon test. */
  sequentialTuning?: number | null;
}

export interface FrequentistResult {
  pointEstimate: number;
  ci: [number, number];
  pValue: number;
  testStatistic: number;
  degreesOfFreedom: number;
  significant: boolean;
  test: "two_sided" | "sequential_two_sided";
}

export const DEFAULT_SEQUENTIAL_TUNING = 5000;
const MIN_SAMPLE = 5;

function pointEstimate(t: Stat, c: Stat, diff: Difference): number {
  const tm = meanOf(t);
  const cm = meanOf(c);
  if (diff === "absolute") return tm - cm;
  if (Math.abs(cm) < 1e-10) throw new StatisticError("Control mean cannot be zero for relative difference calculation");
  return (tm - cm) / cm;
}

function pooledVariance(t: Stat, c: Stat, diff: Difference): number {
  const tv = varianceOf(t);
  const cv = varianceOf(c);
  if (diff === "absolute") return tv / t.n + cv / c.n;
  const tm = meanOf(t);
  const cm = meanOf(c);
  if (Math.abs(cm) < 1e-10) throw new StatisticError("Control mean cannot be zero for relative variance calculation");
  return tv / (t.n * cm ** 2) + (tm ** 2 * cv) / (c.n * cm ** 4);
}

function welchDf(t: Stat, c: Stat): number {
  if (Math.min(t.n, c.n) < 2) throw new StatisticError("Welch-Satterthwaite requires n >= 2 per group.");
  const a = varianceOf(t) / t.n;
  const b = varianceOf(c) / c.n;
  return (a + b) ** 2 / (a ** 2 / (t.n - 1) + b ** 2 / (c.n - 1));
}

export function sequentialRho(alpha: number, tuning: number): number {
  if (!(alpha > 0 && alpha < 1)) throw new StatisticError("Alpha must be between 0 and 1");
  if (!(tuning > 0)) throw new StatisticError("Sequential tuning parameter must be positive");
  const la = Math.log(alpha);
  return Math.sqrt((-2 * la + Math.log(-2 * la + 1)) / tuning);
}

export function sequentialHalfwidth(s2: number, n: number, rho: number, alpha: number): number {
  if (!(s2 > 0)) throw new StatisticError("s2 must be positive");
  if (n < 1) throw new StatisticError("Sample size must be at least 1");
  const k = n * rho ** 2 + 1;
  return Math.sqrt(s2) * Math.sqrt((2 * k * Math.log(Math.sqrt(k) / alpha)) / (n * rho) ** 2);
}

export function sequentialPValue(estimate: number, pooled: number, n: number, rho: number): number {
  if (!(pooled > 0)) throw new StatisticError("Pooled variance must be positive");
  if (n < 1) throw new StatisticError("Sample size must be at least 1");
  const t2 = (estimate ** 2 * n) / pooled;
  const k = n * rho ** 2 + 1;
  const logE = (rho ** 2 * t2) / (2 * k) - 0.5 * Math.log(k);
  if (logE <= 0) return 1;
  return Math.min(Math.exp(-logE), 1);
}

export function frequentistTest(t: Stat, c: Stat, opts: FrequentistOptions): FrequentistResult {
  if (!(opts.alpha > 0 && opts.alpha < 1)) throw new StatisticError("Alpha must be between 0 and 1");
  sameKind(t, c);
  checkStat(t);
  checkStat(c);
  if (t.n < MIN_SAMPLE) throw new StatisticError(`Treatment sample size (${t.n}) below minimum (${MIN_SAMPLE})`);
  if (c.n < MIN_SAMPLE) throw new StatisticError(`Control sample size (${c.n}) below minimum (${MIN_SAMPLE})`);
  const est = pointEstimate(t, c, opts.difference);
  const pooled = pooledVariance(t, c, opts.difference);
  if (opts.sequentialTuning) {
    const rho = sequentialRho(opts.alpha, opts.sequentialTuning);
    const n = t.n + c.n;
    const p = sequentialPValue(est, pooled, n, rho);
    const half = sequentialHalfwidth(pooled * n, n, rho, opts.alpha);
    return { pointEstimate: est, ci: [est - half, est + half], pValue: p, testStatistic: NaN, degreesOfFreedom: NaN, significant: p < opts.alpha, test: "sequential_two_sided" };
  }
  const df = welchDf(t, c);
  if (!(pooled > 0)) throw new StatisticError("Pooled variance must be positive");
  const tStat = est / Math.sqrt(pooled);
  const p = 2 * (1 - studentTCdf(Math.abs(tStat), df));
  const margin = studentTPpf(1 - opts.alpha / 2, df) * Math.sqrt(pooled);
  return { pointEstimate: est, ci: [est - margin, est + margin], pValue: p, testStatistic: tStat, degreesOfFreedom: df, significant: p < opts.alpha, test: "two_sided" };
}

export interface GaussianPrior {
  mean: number;
  variance: number;
  proper: boolean;
}

export interface BayesianOptions {
  difference: Difference;
  ciLevel: number;
  /** Lower is better for this metric. */
  inverse?: boolean;
  prior?: GaussianPrior;
}

export interface BayesianResult {
  effectSize: number;
  ci: [number, number];
  chanceToWin: number;
  posteriorVariance: number;
  decisive: boolean;
}

export const FLAT_PRIOR: GaussianPrior = { mean: 0, variance: 1, proper: false };

function effectAndVariance(t: Stat, c: Stat, diff: Difference): [number, number] {
  const tm = meanOf(t);
  const cm = meanOf(c);
  const tv = varianceOf(t);
  const cv = varianceOf(c);
  if (diff === "absolute") return [tm - cm, tv / t.n + cv / c.n];
  if (cm <= 0) throw new StatisticError("Control mean must be positive for relative difference calculation");
  // Delta method for the ratio of the two means (treatment and control are independent).
  return [(tm - cm) / cm, tv / t.n / cm ** 2 + (tm ** 2 * (cv / c.n)) / cm ** 4];
}

export function bayesianTest(t: Stat, c: Stat, opts: BayesianOptions): BayesianResult {
  if (!(opts.ciLevel > 0 && opts.ciLevel < 1)) throw new StatisticError("ci_level must be between 0 and 1");
  const prior = opts.prior ?? FLAT_PRIOR;
  if (!(prior.variance > 0)) throw new StatisticError("Prior variance must be positive");
  sameKind(t, c);
  checkStat(t);
  checkStat(c);
  if (t.kind === "proportion" && c.kind === "proportion") {
    for (const [name, s] of [["treatment", t], ["control", c]] as const) {
      if (s.sum < 5) throw new StatisticError(`Normal approximation invalid: ${name} has only ${s.sum} successes (need at least 5).`);
      if (s.n - s.sum < 5) throw new StatisticError(`Normal approximation invalid: ${name} has only ${s.n - s.sum} failures (need at least 5).`);
    }
  }
  for (const [name, s] of [["Treatment", t], ["Control", c]] as const) {
    const errors = validationErrors(s);
    if (errors.length) throw new StatisticError(`${name} statistic validation failed: ${errors.join("; ")}`);
  }
  const [effect, variance] = effectAndVariance(t, c, opts.difference);
  if (!(variance > 0)) throw new StatisticError("Effect variance must be positive");
  const dataPrecision = 1 / variance;
  const priorPrecision = prior.proper ? 1 / prior.variance : 0;
  const postPrecision = priorPrecision + dataPrecision;
  const postMean = prior.proper ? (priorPrecision * prior.mean + dataPrecision * effect) / postPrecision : effect;
  const postVar = 1 / postPrecision;
  const sd = Math.sqrt(postVar);
  const chance = opts.inverse ? normalCdf(0, postMean, sd) : normalSf(0, postMean, sd);
  const alpha = 1 - opts.ciLevel;
  const ci: [number, number] = [normalPpf(alpha / 2, postMean, sd), normalPpf(1 - alpha / 2, postMean, sd)];
  return { effectSize: postMean, ci, chanceToWin: chance, posteriorVariance: postVar, decisive: chance > opts.ciLevel || chance < 1 - opts.ciLevel };
}
