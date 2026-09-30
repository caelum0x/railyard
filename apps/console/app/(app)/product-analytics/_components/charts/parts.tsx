// Shared chart parts: legend (always for two or more series), the table
// view (every value reachable without hovering), and the tooltip box.
import type { CSSProperties, ReactNode } from "react";
import { INK } from "./palette.ts";

export interface LegendItem {
  label: string;
  color: string;
  shape?: "line" | "rect";
}

export function Legend({ items }: { items: LegendItem[] }) {
  if (items.length < 2) return null;
  return (
    <ul aria-label="Legend" style={{ listStyle: "none", display: "flex", flexWrap: "wrap", gap: 14, padding: 0, margin: "8px 0 0", fontSize: 13, color: INK.secondary }}>
      {items.map((it) => (
        <li key={it.label} style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
          <svg width="14" height="10" aria-hidden="true">
            {it.shape === "rect" ? <rect x="1" y="1" width="12" height="8" rx="2" fill={it.color} /> : <line x1="1" y1="5" x2="13" y2="5" stroke={it.color} strokeWidth="2" strokeLinecap="round" />}
          </svg>
          {it.label}
        </li>
      ))}
    </ul>
  );
}

export function TableView({ caption, head, rows }: { caption: string; head: string[]; rows: (string | number)[][] }) {
  return (
    <details style={{ marginTop: 10 }}>
      <summary className="muted" style={{ cursor: "pointer", fontSize: 13 }}>
        Show as a table
      </summary>
      <div style={{ overflowX: "auto", maxHeight: 320 }}>
        <table style={{ fontSize: 13, borderCollapse: "collapse", fontVariantNumeric: "tabular-nums" }}>
          <caption className="muted" style={{ textAlign: "left", padding: "6px 0" }}>
            {caption}
          </caption>
          <thead>
            <tr>
              {head.map((h) => (
                <th key={h} style={{ textAlign: "left", padding: "4px 10px 4px 0", color: INK.secondary, fontWeight: 600 }}>
                  {h}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={i}>
                {r.map((c, j) => (
                  <td key={j} style={{ padding: "3px 10px 3px 0", borderTop: `1px solid ${INK.grid}` }}>
                    {c}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}

/** A tooltip at `pct` percent across the figure; it flips to the left of the pointer past 60%. */
export function TooltipBox({ pct, top, children }: { pct: number; top: number; children: ReactNode }) {
  const side: CSSProperties = pct > 60 ? { right: `calc(${(100 - pct).toFixed(2)}% + 12px)` } : { left: `calc(${pct.toFixed(2)}% + 12px)` };
  const style: CSSProperties = {
    position: "absolute",
    ...side,
    top: Math.max(0, top),
    background: "#ffffff",
    border: "1px solid rgba(11,11,11,0.10)",
    borderRadius: 10,
    padding: "8px 10px",
    fontSize: 12,
    color: INK.primary,
    pointerEvents: "none",
    boxShadow: "0 6px 18px rgba(29,36,51,0.12)",
    minWidth: 150,
    zIndex: 2,
  };
  return (
    <div role="status" style={style}>
      {children}
    </div>
  );
}

export function TipRow({ color, label, value }: { color: string; label: string; value: string }) {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 6, lineHeight: 1.6 }}>
      <svg width="12" height="6" aria-hidden="true">
        <line x1="1" y1="3" x2="11" y2="3" stroke={color} strokeWidth="2" strokeLinecap="round" />
      </svg>
      <strong style={{ fontVariantNumeric: "tabular-nums" }}>{value}</strong>
      <span style={{ color: INK.secondary }}>{label}</span>
    </div>
  );
}
