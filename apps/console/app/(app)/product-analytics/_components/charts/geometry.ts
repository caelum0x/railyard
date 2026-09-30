// Pure chart geometry and formatting for the product analytics charts:
// clean tick steps, scales, SVG paths and compact numbers. No DOM, no
// dependencies, so it is tested in node.

export interface Scale {
  (v: number): number;
  domain: [number, number];
  ticks: number[];
}

/** Clean ticks (1, 2 or 5 times a power of ten) from min to at least max, about `count` of them. */
export function niceTicks(minIn: number, maxIn: number, count = 4): number[] {
  const min = Math.min(0, minIn);
  const max = maxIn <= min ? min + 1 : maxIn;
  const raw = (max - min) / Math.max(1, count);
  const pow = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 5, 10].map((m) => m * pow).find((s) => s >= raw) ?? 10 * pow;
  const start = Math.floor(min / step) * step;
  const out: number[] = [];
  for (let v = start; v < max + step - 1e-9; v += step) out.push(Number(v.toPrecision(12)));
  return out;
}

/** A linear scale over clean ticks, from data min/max to pixel range [r0, r1]. */
export function linearScale(values: number[], r0: number, r1: number, count = 4): Scale {
  const finite = values.filter((v) => Number.isFinite(v));
  const lo = finite.length ? Math.min(...finite) : 0;
  const hi = finite.length ? Math.max(...finite) : 1;
  const ticks = niceTicks(lo, hi, count);
  const d0 = ticks[0];
  const d1 = ticks[ticks.length - 1];
  const f = ((v: number) => r0 + ((v - d0) / (d1 - d0 || 1)) * (r1 - r0)) as Scale;
  f.domain = [d0, d1];
  f.ticks = ticks;
  return f;
}

/** Evenly spaced x positions for n points across [x0, x1] (one point sits in the middle). */
export function bandX(n: number, x0: number, x1: number): number[] {
  if (n <= 1) return [(x0 + x1) / 2];
  const step = (x1 - x0) / (n - 1);
  return Array.from({ length: n }, (_, i) => x0 + i * step);
}

export function linePath(xs: number[], ys: number[]): string {
  return xs.map((x, i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${ys[i].toFixed(1)}`).join("");
}

/**
 * A column from the baseline to `top` with 4px rounded data end and a square
 * base. Negative columns (top below the baseline) round their lower end.
 */
export function columnPath(x: number, width: number, base: number, top: number, radius = 4): string {
  const h = Math.abs(base - top);
  const r = Math.min(radius, width / 2, h);
  if (h === 0) return "";
  if (top <= base) {
    return `M${x},${base}V${top + r}Q${x},${top} ${x + r},${top}H${x + width - r}Q${x + width},${top} ${x + width},${top + r}V${base}Z`;
  }
  return `M${x},${base}V${top - r}Q${x},${top} ${x + r},${top}H${x + width - r}Q${x + width},${top} ${x + width},${top - r}V${base}Z`;
}

/** 1,284 / 12.9K / 4.2M; fractions keep two digits. */
export function compact(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "";
  const a = Math.abs(v);
  if (a >= 1e9) return `${(v / 1e9).toFixed(a >= 1e10 ? 0 : 1)}B`;
  if (a >= 1e6) return `${(v / 1e6).toFixed(a >= 1e7 ? 0 : 1)}M`;
  if (a >= 1e4) return `${(v / 1e3).toFixed(a >= 1e5 ? 0 : 1)}K`;
  if (Number.isInteger(v)) return v.toLocaleString("en-US");
  return v.toLocaleString("en-US", { maximumFractionDigits: 2 });
}

export function percent(v: number): string {
  if (!Number.isFinite(v)) return "";
  return `${(v * 100).toFixed(v >= 0.1 || v === 0 ? 0 : 1)}%`;
}

/** 45 s / 12 min / 3.5 h / 2.1 d */
export function duration(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return "";
  if (seconds < 90) return `${Math.round(seconds)} s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)} min`;
  if (seconds < 172_800) return `${(seconds / 3600).toFixed(1)} h`;
  return `${(seconds / 86_400).toFixed(1)} d`;
}

/** Axis label for a bucket: "Sep 3" for days and weeks, "Sep 2026" for months, "14:00" for hours. */
export function bucketText(label: string, interval: "hour" | "day" | "week" | "month"): string {
  const d = new Date(label.length > 10 ? `${label.replace(" ", "T")}:00Z` : `${label}T00:00:00Z`);
  if (Number.isNaN(d.getTime())) return label;
  const month = d.toLocaleString("en-US", { month: "short", timeZone: "UTC" });
  if (interval === "hour") return label.slice(11, 16);
  if (interval === "month") return `${month} ${d.getUTCFullYear()}`;
  return `${month} ${d.getUTCDate()}`;
}

/** At most `max` evenly spread label indexes, always including the first and last. */
export function labelIndexes(n: number, max = 6): number[] {
  if (n <= max) return Array.from({ length: n }, (_, i) => i);
  const step = (n - 1) / (max - 1);
  return [...new Set(Array.from({ length: max }, (_, i) => Math.round(i * step)))];
}

/** Index of the point nearest to pixel x. */
export function nearestIndex(xs: number[], x: number): number {
  let best = 0;
  for (let i = 1; i < xs.length; i += 1) if (Math.abs(xs[i] - x) < Math.abs(xs[best] - x)) best = i;
  return best;
}

/** Ink for text on a filled cell: white on dark fills, near-black on light ones (WCAG relative luminance). */
export function inkOn(hex: string): string {
  const n = parseInt(hex.slice(1), 16);
  const ch = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  });
  const lum = 0.2126 * ch[0] + 0.7152 * ch[1] + 0.0722 * ch[2];
  return lum > 0.35 ? "#0b0b0b" : "#ffffff";
}
