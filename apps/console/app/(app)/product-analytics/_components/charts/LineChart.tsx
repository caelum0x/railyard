"use client";
// Trends over time: 2px lines on recessive hairline grid, crosshair that
// snaps to the nearest bucket, one tooltip listing every series (keyboard:
// focus the plot and use the arrow keys), end labels for up to four series,
// a legend for two or more, and a table view.
import { useState, type KeyboardEvent, type PointerEvent } from "react";
import { bandX, bucketText, compact, labelIndexes, linearScale, linePath, nearestIndex } from "./geometry.ts";
import { INK, seriesColor } from "./palette.ts";
import { Legend, TableView, TipRow, TooltipBox } from "./parts.tsx";

export interface LineSeries {
  label: string;
  data: number[];
  /** A dimmer, thinner line (compare to previous). */
  muted?: boolean;
}

const W = 640;
const H = 240;
const M = { top: 12, right: 96, bottom: 28, left: 48 };

export function LineChart({ labels, series, interval, caption }: { labels: string[]; series: LineSeries[]; interval: "hour" | "day" | "week" | "month"; caption: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const xs = bandX(labels.length, M.left, W - M.right);
  const y = linearScale(series.flatMap((s) => s.data), H - M.bottom, M.top);
  const colors = series.map((s, i) => (s.muted ? INK.muted : seriesColor(i - series.slice(0, i).filter((x) => x.muted).length)));
  const pick = (e: PointerEvent<SVGRectElement>) => {
    const box = e.currentTarget.ownerSVGElement?.getBoundingClientRect();
    if (!box) return;
    setHover(nearestIndex(xs, ((e.clientX - box.left) / box.width) * W));
  };
  const keys = (e: KeyboardEvent<SVGRectElement>) => {
    if (e.key === "ArrowRight") setHover((h) => Math.min(labels.length - 1, (h ?? -1) + 1));
    if (e.key === "ArrowLeft") setHover((h) => Math.max(0, (h ?? labels.length) - 1));
    if (e.key === "Escape") setHover(null);
  };
  const endLabels = series.length <= 4;
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
        {labelIndexes(labels.length).map((i) => (
          <text key={i} x={xs[i]} y={H - 8} textAnchor="middle" fontSize="11" fill={INK.muted}>
            {bucketText(labels[i], interval)}
          </text>
        ))}
        {series.map((s, i) => (
          <path key={`${s.label}-${i}`} d={linePath(xs, s.data.map((v) => y(v)))} fill="none" stroke={colors[i]} strokeWidth={s.muted ? 1.5 : 2} strokeLinejoin="round" strokeLinecap="round" />
        ))}
        {endLabels
          ? series.map((s, i) => (
              <text key={`end-${i}`} x={xs[xs.length - 1] + 8} y={y(s.data[s.data.length - 1] ?? 0) + 4} fontSize="11" fill={INK.secondary}>
                {compact(s.data[s.data.length - 1])}
              </text>
            ))
          : null}
        {hover !== null ? (
          <g>
            <line x1={xs[hover]} x2={xs[hover]} y1={M.top} y2={H - M.bottom} stroke={INK.axis} strokeWidth="1" />
            {series.map((s, i) => (
              <circle key={i} cx={xs[hover]} cy={y(s.data[hover] ?? 0)} r="4" fill={colors[i]} stroke={INK.surface} strokeWidth="2" />
            ))}
          </g>
        ) : null}
        <rect
          x={M.left - 10}
          y={0}
          width={W - M.left - M.right + 20}
          height={H}
          fill="transparent"
          tabIndex={0}
          aria-label="Move through the chart with the arrow keys"
          onPointerMove={pick}
          onPointerLeave={() => setHover(null)}
          onKeyDown={keys}
          onBlur={() => setHover(null)}
        />
      </svg>
      {hover !== null ? (
        <TooltipBox pct={(xs[hover] / W) * 100} top={M.top}>
          <div style={{ color: INK.secondary, marginBottom: 2 }}>{labels[hover]}</div>
          {series.map((s, i) => (
            <TipRow key={i} color={colors[i]} label={s.label} value={compact(s.data[hover])} />
          ))}
        </TooltipBox>
      ) : null}
      <Legend items={series.map((s, i) => ({ label: s.label, color: colors[i] }))} />
      <TableView caption={caption} head={["Period", ...series.map((s) => s.label)]} rows={labels.map((l, i) => [l, ...series.map((s) => compact(s.data[i]))])} />
    </figure>
  );
}
