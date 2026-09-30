// Renders any insight answer with the matching chart. Trends with a
// breakdown or several series fold past the eighth line into the table
// (the eight categorical slots are never cycled).
import { BarChart } from "./BarChart.tsx";
import { FunnelChart } from "./FunnelChart.tsx";
import { bucketText, compact } from "./geometry.ts";
import { LineChart, type LineSeries } from "./LineChart.tsx";
import { LIFECYCLE, seriesColor } from "./palette.ts";
import { PathsList } from "./PathsList.tsx";
import { RetentionGrid } from "./RetentionGrid.tsx";
import type { Result } from "../../_lib/insights-client.ts";

export const MAX_LINES = 8;

export function ResultView({ result, dropOffBase, compactView = false }: { result: Result; dropOffBase?: string; compactView?: boolean }) {
  switch (result.kind) {
    case "trends": {
      const lines: LineSeries[] = result.series.slice(0, MAX_LINES).map((s) => ({ label: s.breakdown ? `${s.label}: ${s.breakdown}` : s.label, data: s.data }));
      if (result.previous && result.series.length === 1) lines.push({ label: "Previous period", data: result.previous.series[0]?.data ?? [], muted: true });
      const single = result.series.length === 1 ? result.series[0] : null;
      return (
        <>
          {single && single.total !== null && !compactView ? (
            <p style={{ fontSize: 28, fontWeight: 600, margin: "0 0 8px" }} data-copy-source="product-analytics insights query (live)">
              {compact(single.total)}
            </p>
          ) : null}
          <LineChart labels={result.labels} series={lines} interval={result.interval} caption="Trend over time" />
          {result.series.length > MAX_LINES ? <p className="muted">The chart shows the first {MAX_LINES} lines; the table lists all of them.</p> : null}
        </>
      );
    }
    case "funnels":
      return (
        <FunnelChart
          caption="Funnel conversion by step"
          dropOffBase={dropOffBase}
          groups={result.groups.slice(0, MAX_LINES).map((g) => ({ label: g.breakdown ?? "Everyone", steps: g.steps }))}
        />
      );
    case "retention":
      return <RetentionGrid rows={result.cohorts} period={result.period} caption="Share of the people who started in each period that came back" />;
    case "paths":
      return <PathsList edges={result.edges} caption="Most travelled steps" limit={compactView ? 8 : 30} />;
    case "stickiness":
      // One chart per series (small multiples): stacking different events would add unlike counts.
      return (
        <>
          {result.series.slice(0, MAX_LINES).map((s, i) => (
            <div key={s.label} style={{ marginBottom: 12 }}>
              {result.series.length > 1 ? <h3 style={{ fontSize: 14, margin: "0 0 4px" }}>{s.label}</h3> : null}
              <BarChart
                caption={`People by number of active ${result.interval}s: ${s.label}`}
                labels={s.data.map((_, n) => `${n + 1} ${result.interval}${n === 0 ? "" : "s"}`)}
                parts={[{ label: s.label, color: seriesColor(i), data: s.data }]}
              />
            </div>
          ))}
        </>
      );
    case "lifecycle":
      return (
        <BarChart
          caption="New, returning, resurrecting and dormant people"
          labels={result.labels.map((l) => bucketText(l, result.interval))}
          parts={(["new", "returning", "resurrecting", "dormant"] as const).map((k) => ({ label: k[0].toUpperCase() + k.slice(1), color: LIFECYCLE[k], data: result.statuses[k] }))}
        />
      );
  }
}
