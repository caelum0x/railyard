/**
 * Formatting helpers for the per-key trace view (RY-RL3). Kept pure so they
 * are unit-tested without rendering.
 */
import type { TraceScope, TraceSummary } from "~/types/traces";

export const MICROS_PER_USD = 1_000_000;
/** Look-back windows offered in the UI (hours). 2160 h is the 90-day cap. */
export const WINDOW_OPTIONS: ReadonlyArray<{ hours: number; label: string }> = [
  { hours: 1, label: "Last hour" },
  { hours: 24, label: "Last 24 hours" },
  { hours: 168, label: "Last 7 days" },
  { hours: 720, label: "Last 30 days" },
  { hours: 2160, label: "Last 90 days" },
];
export const DEFAULT_WINDOW_HOURS = 168;
/** The first page refreshes on this interval so a new call shows within 10 s. */
export const REFRESH_INTERVAL_MS = 5_000;

export function formatTraceCost(micros: number | undefined): string {
  if (micros == null) {
    return "unpriced";
  }
  return (micros / MICROS_PER_USD).toLocaleString("en-US", {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
    maximumFractionDigits: 6,
  });
}

export function formatTokens(value: number | undefined): string {
  return value == null ? "-" : value.toLocaleString("en-US");
}

export function formatLatency(ms: number | undefined): string {
  if (ms == null) {
    return "-";
  }
  return ms >= 1000 ? `${(ms / 1000).toFixed(2)} s` : `${ms} ms`;
}

export function formatTraceTime(createdAtUs: number): string {
  return new Date(Math.floor(createdAtUs / 1000)).toISOString();
}

/** One-word status for a trace row. */
export function traceStatus(
  trace: Pick<TraceSummary, "model_calls" | "finish_reason">,
): string {
  if (trace.model_calls === 0) {
    return "no model call stored";
  }
  return trace.finish_reason ?? "unknown";
}

export interface TraceFilters {
  scope: TraceScope;
  key?: string;
  windowHours: number;
  before?: string;
  functionName?: string;
}

/** Parse the page URL into validated filters. Unknown values fall back. */
export function filtersFromUrl(url: string | URL): TraceFilters {
  const parsed = typeof url === "string" ? new URL(url) : url;
  const params = parsed.searchParams;
  const scope: TraceScope = params.get("scope") === "all" ? "all" : "own";
  const rawWindow = Number(params.get("window_hours"));
  const windowHours = WINDOW_OPTIONS.some(
    (option) => option.hours === rawWindow,
  )
    ? rawWindow
    : DEFAULT_WINDOW_HOURS;
  const key = params.get("key")?.trim() || undefined;
  const before = params.get("before")?.trim() || undefined;
  const functionName = params.get("function")?.trim() || undefined;
  return {
    scope,
    key: scope === "all" ? key : undefined,
    windowHours,
    before,
    functionName,
  };
}

/** Build a page URL for the given filters (used by pagination links). */
export function hrefForFilters(
  pathname: string,
  filters: TraceFilters,
): string {
  const params = new URLSearchParams();
  if (filters.scope === "all") {
    params.set("scope", "all");
    if (filters.key) {
      params.set("key", filters.key);
    }
  }
  if (filters.windowHours !== DEFAULT_WINDOW_HOURS) {
    params.set("window_hours", String(filters.windowHours));
  }
  if (filters.functionName) {
    params.set("function", filters.functionName);
  }
  if (filters.before) {
    params.set("before", filters.before);
  }
  const query = params.toString();
  return `${pathname}${query ? `?${query}` : ""}`;
}
