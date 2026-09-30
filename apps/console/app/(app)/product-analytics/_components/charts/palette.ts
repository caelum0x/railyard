// Chart colors by job (validated with the dataviz validator against the
// console surface #f4f6fa: categorical order passes every adjacent check;
// four slots sit under 3:1, so every chart ships a table view and labels).
// Text never wears a series color; it uses the ink tokens.

/** Categorical slots in fixed order (never cycled; past eight, fold into "(other)"). */
export const SERIES = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#4a3aa7", "#e34948"];

/** One-hue blue ramp, light to dark, for magnitude (retention cells). */
export const SEQUENTIAL = ["#cde2fb", "#b7d3f6", "#9ec5f4", "#86b6ef", "#6da7ec", "#5598e7", "#3987e5", "#2a78d6", "#256abf", "#1c5cab", "#184f95", "#104281", "#0d366b"];

export const INK = {
  primary: "#0b0b0b",
  secondary: "#52514e",
  muted: "#898781",
  grid: "#e1e0d9",
  axis: "#c3c2b7",
  surface: "#fcfcfb",
};

/** Lifecycle states keep one color each wherever they appear. */
export const LIFECYCLE = { new: SERIES[0], returning: SERIES[2], resurrecting: SERIES[6], dormant: SERIES[1] } as const;

export function seriesColor(i: number): string {
  return SERIES[Math.min(i, SERIES.length - 1)];
}

/** A sequential step for a share in [0, 1]. */
export function sequentialFor(share: number): string {
  if (!Number.isFinite(share) || share <= 0) return INK.surface;
  const i = Math.min(SEQUENTIAL.length - 1, Math.floor(share * SEQUENTIAL.length));
  return SEQUENTIAL[i];
}
