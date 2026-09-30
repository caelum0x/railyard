// Replay and heatmap limits (copy-source: apps/product-analytics/src/replay/limits.ts).
// Every value is a target until the owner sets it (PLAN section 8).

import type { Site } from "../tenancy/site-directory.ts";

/** Days a recording is kept (target), never longer than the plan keeps events. */
export const REPLAY_RETENTION_DAYS = 30;
/** Share of sessions recorded when a project turns recording on (target). */
export const DEFAULT_SAMPLE_RATE = 0.1;
/** A recording stops being stored after this long (target: 30 minutes). */
export const MAX_RECORDING_MS = 30 * 60_000;
/** URLs kept in a recording's index row. */
export const MAX_INDEX_URLS = 20;

export function replayRetentionDays(site: Pick<Site, "retention_days">): number {
  return Math.max(1, Math.min(REPLAY_RETENTION_DAYS, site.retention_days));
}
