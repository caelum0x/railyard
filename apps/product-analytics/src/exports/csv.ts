// CSV of an insight's answer: one table per insight type, in the same cells
// the console shows, through the SQL screen's CSV writer (formula injection
// guarded). A query or a saved insight goes through the insights cache, so a
// download costs no more than looking at the chart.

import type { InsightResult } from "../insights/runner.ts";
import { toCsv } from "../insights/sandbox.ts";

export function insightTable(r: InsightResult): { columns: string[]; rows: unknown[][] } {
  switch (r.kind) {
    case "trends": {
      const columns = ["series", "breakdown", ...r.labels, "total"];
      const rows = r.series.map((s) => [s.label, s.breakdown ?? "", ...s.data, s.total ?? s.data.reduce((a, b) => a + b, 0)]);
      if (r.previous) for (const s of r.previous.series) rows.push([`${s.label} (previous period)`, s.breakdown ?? "", ...s.data, s.total ?? s.data.reduce((a, b) => a + b, 0)]);
      return { columns, rows };
    }
    case "funnels":
      return {
        columns: ["breakdown", "step", "name", "people", "conversion_from_first", "conversion_from_previous", "dropped", "avg_seconds", "median_seconds"],
        rows: r.groups.flatMap((g) => g.steps.map((s) => [g.breakdown_value ?? "", s.step, s.name, s.people, s.from_first, s.from_previous, s.dropped, s.avg_seconds, s.median_seconds])),
      };
    case "retention": {
      const n = Math.max(0, ...r.cohorts.map((c) => c.values.length));
      return { columns: ["cohort", "size", ...Array.from({ length: n }, (_, i) => `${r.period}_${i}`)], rows: r.cohorts.map((c) => [c.label, c.size, ...c.values]) };
    }
    case "paths":
      return { columns: ["step", "from", "to", "people", "avg_seconds", "ended_here"], rows: r.edges.map((e) => [e.source_step, e.source, e.target, e.people, e.avg_seconds, e.ends]) };
    case "stickiness": {
      const n = Math.max(0, ...r.series.map((s) => s.data.length));
      return { columns: ["series", ...Array.from({ length: n }, (_, i) => `${i + 1}_${r.interval}s`)], rows: r.series.map((s) => [s.label, ...s.data]) };
    }
    case "lifecycle":
      return { columns: ["status", ...r.labels], rows: (["new", "returning", "resurrecting", "dormant"] as const).map((k) => [k, ...r.statuses[k]]) };
  }
}

export const insightCsv = (r: InsightResult): string => toCsv({ ...insightTable(r), truncated: false });
