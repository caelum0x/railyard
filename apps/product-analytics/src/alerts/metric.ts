// The one number an alert watches on a saved insight (QY1.4, A25), and the
// numbers before it for an anomaly check. The latest interval of a trend is
// still filling, so the watched value is the last complete interval.

import type { InsightResult } from "../insights/runner.ts";

export interface Headline {
  /** The watched value, or null when the insight has none yet. */
  value: number | null;
  /** Earlier complete intervals, oldest first (trends and lifecycle only). */
  history: number[];
  /** What the value is, in words. */
  label: string;
}

const round = (n: number) => Math.round(n * 1e6) / 1e6;

/** The headline of an insight's answer for one series. */
export function headline(result: InsightResult, seriesIndex = 0): Headline {
  switch (result.kind) {
    case "trends": {
      const lines = result.series.filter((s) => s.series_index === seriesIndex);
      if (lines.length === 0) return { value: null, history: [], label: "no such series" };
      const buckets = result.labels.map((_, i) => lines.reduce((a, l) => a + (l.data[i] ?? 0), 0));
      const at = buckets.length >= 2 ? buckets.length - 2 : buckets.length - 1;
      return at < 0 ? { value: null, history: [], label: lines[0].label } : { value: round(buckets[at]), history: buckets.slice(0, at).map(round), label: `${lines[0].label}, ${result.labels[at]}` };
    }
    case "lifecycle": {
      const s = result.statuses.new;
      const at = s.length >= 2 ? s.length - 2 : s.length - 1;
      return at < 0 ? { value: null, history: [], label: "new people" } : { value: s[at], history: s.slice(0, at), label: `new people, ${result.labels[at]}` };
    }
    case "funnels": {
      const steps = result.groups[0]?.steps ?? [];
      const first = steps[0]?.people ?? 0;
      const last = steps[steps.length - 1]?.people ?? 0;
      return { value: first ? round((last / first) * 100) : null, history: [], label: "conversion, percent" };
    }
    case "retention": {
      const c = [...result.cohorts].reverse().find((x) => x.values.length > 1 && x.size > 0);
      return { value: c ? round((c.values[1] / c.size) * 100) : null, history: [], label: c ? `returned after one ${result.period}, ${c.label}, percent` : "returned, percent" };
    }
    case "stickiness": {
      const s = result.series[seriesIndex];
      return { value: s ? s.data.reduce((a, b) => a + b, 0) : null, history: [], label: s ? s.label : "no such series" };
    }
    case "paths": {
      const people = result.edges.filter((e) => e.source_step === 1).reduce((a, e) => a + e.people, 0);
      return { value: people, history: [], label: "people on the first step" };
    }
  }
}

/** How far the value is from the history, in standard deviations (null with fewer than 7 earlier points). */
export function zScore(value: number, history: number[]): number | null {
  if (history.length < 7) return null;
  const mean = history.reduce((a, b) => a + b, 0) / history.length;
  const sd = Math.sqrt(history.reduce((a, b) => a + (b - mean) ** 2, 0) / (history.length - 1));
  if (sd === 0) return value === mean ? 0 : value > mean ? Infinity : -Infinity;
  return (value - mean) / sd;
}
