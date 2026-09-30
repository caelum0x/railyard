"use client";
// Funnel: one horizontal bar per step (at most 24px thick, 4px rounded
// end), converted in the series color and the drop-off as a light wash of
// the same hue behind it, conversion and median time as text. With a
// breakdown, each value is its own row group in categorical order.
import { useState } from "react";
import { compact, duration, percent } from "./geometry.ts";
import { INK, seriesColor } from "./palette.ts";
import { Legend, TableView } from "./parts.tsx";

export interface FunnelStepView {
  name: string;
  people: number;
  from_first: number;
  from_previous: number;
  dropped: number;
  median_seconds: number | null;
}

export interface FunnelGroupView {
  label: string;
  steps: FunnelStepView[];
}

/** `dropOffBase`: a link prefix; "&step=N" is added to list who dropped off before step N. */
export function FunnelChart({ groups, caption, dropOffBase }: { groups: FunnelGroupView[]; caption: string; dropOffBase?: string }) {
  const [hover, setHover] = useState<string | null>(null);
  const top = Math.max(1, ...groups.map((g) => g.steps[0]?.people ?? 0));
  const many = groups.length > 1;
  return (
    <figure style={{ margin: 0 }}>
      <ol style={{ listStyle: "none", padding: 0, margin: 0, display: "grid", gap: 14 }} aria-label={caption}>
        {(groups[0]?.steps ?? []).map((s, i) => (
          <li key={i}>
            <div style={{ display: "flex", justifyContent: "space-between", fontSize: 13, marginBottom: 4 }}>
              <span>
                {i + 1}. {s.name}
              </span>
              {!many ? (
                <span className="muted" style={{ fontVariantNumeric: "tabular-nums" }}>
                  {compact(s.people)} people · {percent(s.from_first)}
                  {i > 0 && s.median_seconds !== null ? ` · median ${duration(s.median_seconds)} from the step before` : ""}
                </span>
              ) : null}
            </div>
            {groups.map((g, gi) => {
              const st = g.steps[i];
              const key = `${gi}:${i}`;
              const color = seriesColor(gi);
              const w = (st.people / top) * 100;
              const prevW = i === 0 ? w : (g.steps[i - 1].people / top) * 100;
              return (
                <div
                  key={key}
                  tabIndex={0}
                  onPointerEnter={() => setHover(key)}
                  onPointerLeave={() => setHover(null)}
                  onFocus={() => setHover(key)}
                  onBlur={() => setHover(null)}
                  aria-label={`${g.label}: ${compact(st.people)} people, ${percent(st.from_first)} of the first step`}
                  style={{ position: "relative", height: 20, marginBottom: 2 }}
                >
                  <svg width="100%" height="20" aria-hidden="true" style={{ display: "block" }}>
                    <rect x="0" y="0" width={`${prevW}%`} height="20" rx="4" fill={color} opacity="0.12" />
                    {w > 0 ? <rect x="0" y="0" width={`${w}%`} height="20" rx="4" fill={color} opacity={hover === null || hover === key ? 1 : 0.6} /> : null}
                  </svg>
                  {hover === key ? (
                    <div role="status" style={{ position: "absolute", right: 0, top: -2, background: "#fff", border: "1px solid rgba(11,11,11,0.10)", borderRadius: 8, padding: "2px 8px", fontSize: 12 }}>
                      <strong>{compact(st.people)}</strong> <span style={{ color: INK.secondary }}>{many ? `${g.label} · ` : ""}{percent(st.from_previous)} of the step before · {compact(st.dropped)} dropped</span>
                    </div>
                  ) : null}
                </div>
              );
            })}
            {i > 0 && dropOffBase && !many ? (
              <a className="muted" style={{ fontSize: 12 }} href={`${dropOffBase}&step=${i + 1}`}>
                {compact(s.dropped)} dropped off before this step
              </a>
            ) : null}
          </li>
        ))}
      </ol>
      {many ? <Legend items={groups.map((g, gi) => ({ label: g.label, color: seriesColor(gi), shape: "rect" }))} /> : null}
      <TableView
        caption={caption}
        head={["Group", "Step", "People", "Of first step", "Of step before", "Dropped", "Median time from the step before"]}
        rows={groups.flatMap((g) => g.steps.map((s, i) => [g.label, `${i + 1}. ${s.name}`, compact(s.people), percent(s.from_first), percent(s.from_previous), compact(s.dropped), duration(s.median_seconds)]))}
      />
    </figure>
  );
}
