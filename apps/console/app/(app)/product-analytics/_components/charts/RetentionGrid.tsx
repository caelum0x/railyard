// Retention: an HTML heat grid. Each row is the people who started in one
// period; column 0 is how many, then the share of them that came back in each later period, on a one-hue
// sequential ramp with ink chosen by the cell's luminance. It is already a
// table, so it is its own table view.
import { inkOn } from "./geometry.ts";
import { INK, sequentialFor } from "./palette.ts";

export interface RetentionRow {
  label: string;
  size: number;
  values: number[];
}

export function RetentionGrid({ rows, period, caption }: { rows: RetentionRow[]; period: "day" | "week" | "month"; caption: string }) {
  const cols = Math.max(1, ...rows.map((r) => r.values.length));
  const unit = period === "day" ? "Day" : period === "week" ? "Week" : "Month";
  return (
    <div style={{ overflowX: "auto" }}>
      <table style={{ borderCollapse: "separate", borderSpacing: 2, fontSize: 12, fontVariantNumeric: "tabular-nums" }}>
        <caption className="muted" style={{ textAlign: "left", paddingBottom: 6 }}>
          {caption}
        </caption>
        <thead>
          <tr>
            <th style={{ textAlign: "left", color: INK.secondary, padding: "2px 8px" }}>Started</th>
            <th style={{ textAlign: "right", color: INK.secondary, padding: "2px 8px" }}>People</th>
            {Array.from({ length: cols - 1 }, (_, k) => (
              <th key={k} style={{ color: INK.secondary, padding: "2px 6px", fontWeight: 600 }}>
                {unit} {k + 1}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.label}>
              <th scope="row" style={{ textAlign: "left", fontWeight: 500, padding: "2px 8px", whiteSpace: "nowrap" }}>
                {r.label}
              </th>
              <td style={{ textAlign: "right", padding: "2px 8px" }}>{r.size.toLocaleString("en-US")}</td>
              {Array.from({ length: cols - 1 }, (_, k) => {
                const v = r.values[k + 1];
                if (v === undefined) return <td key={k} />;
                const share = r.size ? v / r.size : 0;
                const bg = sequentialFor(share);
                return (
                  <td key={k} title={`${v.toLocaleString("en-US")} of ${r.size.toLocaleString("en-US")} people`} style={{ background: bg, color: inkOn(bg), textAlign: "center", padding: "4px 6px", borderRadius: 4, minWidth: 44 }}>
                    {r.size ? `${Math.round(share * 100)}%` : ""}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
