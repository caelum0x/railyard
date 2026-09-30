"use client";
// Columns (stickiness, time to convert, lifecycle): at most 24px wide with a
// 4px rounded data end and a square base, a 2px surface gap between stacked
// parts, per-column hover and focus tooltips, the tallest column labelled,
// a legend for two or more parts and a table view. Negative parts
// (lifecycle "dormant") grow down from the same baseline.
import { useState } from "react";
import { columnPath, compact, labelIndexes, linearScale } from "./geometry.ts";
import { INK } from "./palette.ts";
import { Legend, TableView, TipRow, TooltipBox } from "./parts.tsx";

export interface BarPart {
  label: string;
  color: string;
  data: number[];
}

const W = 640;
const H = 240;
const M = { top: 16, right: 16, bottom: 28, left: 48 };
const GAP = 2;

export function BarChart({ labels, parts, caption }: { labels: string[]; parts: BarPart[]; caption: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const pos = labels.map((_, i) => parts.reduce((a, p) => a + Math.max(0, p.data[i] ?? 0), 0));
  const neg = labels.map((_, i) => parts.reduce((a, p) => a + Math.min(0, p.data[i] ?? 0), 0));
  const y = linearScale([...pos, ...neg, 0], H - M.bottom, M.top);
  const band = (W - M.left - M.right) / Math.max(1, labels.length);
  const bw = Math.max(2, Math.min(24, band * 0.7));
  const base = y(0);
  const tallest = pos.indexOf(Math.max(...pos));
  const fmt = (l: string) => l;
  return (
    <figure style={{ margin: 0, position: "relative" }}>
      <svg viewBox={`0 0 ${W} ${H}`} width="100%" role="img" aria-label={caption} style={{ display: "block", overflow: "visible" }}>
        {y.ticks.map((t) => (
          <g key={t}>
            <line x1={M.left} x2={W - M.right} y1={y(t)} y2={y(t)} stroke={t === 0 ? INK.axis : INK.grid} strokeWidth="1" />
            <text x={M.left - 8} y={y(t) + 4} textAnchor="end" fontSize="11" fill={INK.muted} style={{ fontVariantNumeric: "tabular-nums" }}>
              {compact(t)}
            </text>
          </g>
        ))}
        {labels.map((l, i) => {
          const x = M.left + band * i + (band - bw) / 2;
          let up = base;
          let down = base;
          const shapes = parts.map((p, j) => {
            const v = p.data[i] ?? 0;
            if (v === 0) return null;
            const h = Math.abs(y(v) - base);
            const from = v > 0 ? up : down;
            const to = v > 0 ? from - h : from + h;
            if (v > 0) up = to - GAP;
            else down = to + GAP;
            const last = v > 0 ? parts.slice(j + 1).every((q) => (q.data[i] ?? 0) <= 0) : parts.slice(j + 1).every((q) => (q.data[i] ?? 0) >= 0);
            const d = last ? columnPath(x, bw, from, to) : `M${x},${from}V${to}H${x + bw}V${from}Z`;
            return <path key={p.label} d={d} fill={p.color} opacity={hover === null || hover === i ? 1 : 0.55} />;
          });
          return (
            <g key={l}>
              {shapes}
              <rect
                x={M.left + band * i}
                y={M.top}
                width={band}
                height={H - M.top - M.bottom}
                fill="transparent"
                tabIndex={0}
                aria-label={`${fmt(l)}: ${parts.map((p) => `${p.label} ${compact(p.data[i])}`).join(", ")}`}
                onPointerEnter={() => setHover(i)}
                onPointerLeave={() => setHover(null)}
                onFocus={() => setHover(i)}
                onBlur={() => setHover(null)}
              />
            </g>
          );
        })}
        {labelIndexes(labels.length, 8).map((i) => (
          <text key={i} x={M.left + band * i + band / 2} y={H - 8} textAnchor="middle" fontSize="11" fill={INK.muted}>
            {fmt(labels[i])}
          </text>
        ))}
        {tallest >= 0 && pos[tallest] > 0 ? (
          <text x={M.left + band * tallest + band / 2} y={y(pos[tallest]) - 6} textAnchor="middle" fontSize="11" fill={INK.secondary}>
            {compact(pos[tallest])}
          </text>
        ) : null}
      </svg>
      {hover !== null ? (
        <TooltipBox pct={((M.left + band * hover + band / 2) / W) * 100} top={M.top}>
          <div style={{ color: INK.secondary, marginBottom: 2 }}>{fmt(labels[hover])}</div>
          {parts.map((p) => (
            <TipRow key={p.label} color={p.color} label={p.label} value={compact(p.data[hover])} />
          ))}
        </TooltipBox>
      ) : null}
      <Legend items={parts.map((p) => ({ label: p.label, color: p.color, shape: "rect" }))} />
      <TableView caption={caption} head={["", ...parts.map((p) => p.label)]} rows={labels.map((l, i) => [fmt(l), ...parts.map((p) => compact(p.data[i]))])} />
    </figure>
  );
}
