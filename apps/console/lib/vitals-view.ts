// Page speed per app with deploy markers (Wave P PY3.3) as a pure model, for
// the console page and the Scribase Analytics tab (through the bridge).
// Markers come from the console's own deploy records, so a deploy shows up
// as soon as it is recorded (the pages refresh within 60 s).

import type { InsightsVitals, VitalMetric, VitalRating, VitalsRow } from "./insights.ts";
import type { Deploy } from "./types.ts";

export const METRICS: { key: VitalMetric; label: string; unit: "ms" | "" }[] = [
  { key: "lcp", label: "Largest paint (LCP)", unit: "ms" },
  { key: "inp", label: "Response to input (INP)", unit: "ms" },
  { key: "cls", label: "Layout shift (CLS)", unit: "" },
  { key: "fcp", label: "First paint (FCP)", unit: "ms" },
  { key: "ttfb", label: "Server response (TTFB)", unit: "ms" },
];
const RATING: Record<VitalRating, string> = { good: "good", "needs-improvement": "needs work", poor: "poor" };
/** A change under this share of the earlier value is shown as "about the same". */
const NOISE = 0.1;

export interface MetricCell {
  key: VitalMetric;
  label: string;
  value: string;
  rating: string | null;
}

export interface DeployMarker {
  id: string;
  at: string;
  branch: string;
  url: string;
  /** Plain lines such as "Largest paint (LCP) 1300 ms to 3750 ms, slower". */
  changes: string[];
  /** The metric that got worse the most, if any got worse beyond noise. */
  worse: VitalMetric | null;
}

export interface VitalsView {
  samples: number;
  metrics: MetricCell[];
  daily: { day: string; samples: number; lcp: string; inp: string; cls: string }[];
  deploys: DeployMarker[];
  empty: boolean;
}

export function format(key: VitalMetric, v: number | null): string {
  if (v === null) return "no data";
  return key === "cls" ? v.toFixed(2) : `${Math.round(v)} ms`;
}

/** The deploy times to compare: the app's successful deploys in the window, newest first, at most 10. */
export function markTimes(deploys: Deploy[], appId: string, since: Date): Deploy[] {
  return deploys
    .filter((d) => d.app_id === appId && d.status === "succeeded" && Date.parse(d.created_at) >= since.getTime())
    .sort((a, b) => (a.created_at < b.created_at ? 1 : -1))
    .slice(0, 10);
}

function change(key: VitalMetric, label: string, before: number | null, after: number | null): { line: string; worse: number } | null {
  if (before === null || after === null) return null;
  const base = Math.max(before, key === "cls" ? 0.01 : 1);
  const delta = (after - before) / base;
  const trend = Math.abs(delta) < NOISE ? "about the same" : delta > 0 ? "slower" : "faster";
  const word = key === "cls" && trend !== "about the same" ? (delta > 0 ? "more shifting" : "less shifting") : trend;
  return { line: `${label} ${format(key, before)} to ${format(key, after)}, ${word}`, worse: delta >= NOISE ? delta : 0 };
}

export function vitalsView(v: InsightsVitals, appId: string, deploys: Deploy[]): VitalsView {
  const row: VitalsRow | undefined = v.apps.find((a) => a.app_id === appId);
  const metrics = METRICS.map((m) => ({ key: m.key, label: m.label, value: format(m.key, row?.[m.key].p75 ?? null), rating: row?.[m.key].rating ? RATING[row[m.key].rating as VitalRating] : null }));
  const daily = v.daily
    .filter((d) => d.app_id === appId)
    .map((d) => ({ day: d.day, samples: d.samples, lcp: format("lcp", d.lcp.p75), inp: format("inp", d.inp.p75), cls: format("cls", d.cls.p75) }));
  const byTime = new Map(v.deploys.map((d) => [new Date(d.at).toISOString(), d]));
  const markers = deploys.map((d) => {
    const cmp = byTime.get(new Date(d.created_at).toISOString());
    let worst = 0;
    let worse: VitalMetric | null = null;
    const changes: string[] = [];
    for (const m of METRICS) {
      const c = change(m.key, m.label, cmp?.before?.[m.key].p75 ?? null, cmp?.after?.[m.key].p75 ?? null);
      if (!c) continue;
      changes.push(c.line);
      if (c.worse > worst) {
        worst = c.worse;
        worse = m.key;
      }
    }
    return { id: d.id, at: d.created_at, branch: d.branch, url: d.url, changes, worse };
  });
  return { samples: row?.samples ?? 0, metrics, daily, deploys: markers, empty: !row || row.samples === 0 };
}
