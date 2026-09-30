// Paths: the most travelled steps as a ranked list of edges. Each edge has
// a ribbon whose length is its share of the busiest edge, the step numbers,
// and the average time between the two items.
import { compact, duration } from "./geometry.ts";
import { INK, SERIES } from "./palette.ts";
import { TableView } from "./parts.tsx";

export interface PathEdge {
  source_step: number;
  source: string;
  target: string;
  people: number;
  avg_seconds: number | null;
  ends: number;
}

export function PathsList({ edges, caption, limit = 30 }: { edges: PathEdge[]; caption: string; limit?: number }) {
  const top = Math.max(1, ...edges.map((e) => e.people));
  const shown = edges.slice(0, limit);
  return (
    <figure style={{ margin: 0 }}>
      <ol style={{ listStyle: "none", padding: 0, margin: 0, display: "grid", gap: 8 }} aria-label={caption}>
        {shown.map((e, i) => (
          <li key={`${e.source_step}-${e.source}-${e.target}-${i}`} style={{ fontSize: 13 }}>
            <div style={{ display: "flex", justifyContent: "space-between", gap: 12 }}>
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={`${e.source} to ${e.target}`}>
                <span className="muted">{e.source_step}.</span> {e.source} <span className="muted">to</span> {e.target}
              </span>
              <span className="muted" style={{ whiteSpace: "nowrap", fontVariantNumeric: "tabular-nums" }}>
                {compact(e.people)}
                {e.avg_seconds !== null ? ` · ${duration(e.avg_seconds)}` : ""}
              </span>
            </div>
            <svg width="100%" height="8" aria-hidden="true" style={{ display: "block", marginTop: 3 }}>
              <rect x="0" y="0" width={`${(e.people / top) * 100}%`} height="8" rx="4" fill={SERIES[0]} opacity={e.source_step === 1 ? 1 : Math.max(0.45, 1 - (e.source_step - 1) * 0.15)} />
            </svg>
          </li>
        ))}
      </ol>
      {edges.length > limit ? (
        <p className="muted" style={{ fontSize: 12, color: INK.muted }}>
          The table lists all {edges.length} links.
        </p>
      ) : null}
      <TableView caption={caption} head={["Step", "From", "To", "Sessions", "Average time", "Ended there"]} rows={edges.map((e) => [e.source_step, e.source, e.target, compact(e.people), duration(e.avg_seconds), compact(e.ends)])} />
    </figure>
  );
}
