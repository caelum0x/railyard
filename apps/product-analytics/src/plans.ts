// Product analytics limits per plan (copy-source: apps/product-analytics/src/plans.ts).
// The console reads these from the service; no copy hard-codes them.
// Every value here is a target until the owner sets it (PLAN section 8).

/** The console's plan names (railyard.entitlements.plan). */
export type PlanName = "none" | "trial" | "railyard" | "addon";

/** The Railyard plan's monthly stored-events limit, kept equal to console lib/plans.ts (drift-tested). */
export const RAILYARD_EVENTS_LIMIT = 100_000;

/** Days events are kept, per plan (target). */
export const RETENTION_DAYS: Record<PlanName, number> = {
  none: 30,
  trial: 30,
  railyard: 365,
  addon: 365,
};

/** The longest retention of any plan: whole monthly partitions older than this (plus one month) are dropped. */
export const LONGEST_RETENTION_DAYS = Math.max(...Object.values(RETENTION_DAYS));

/** How long a privacy deletion may take from request to done. */
export const GDPR_DELETE_WITHIN_MS = 24 * 3600 * 1000;

/** Receipts only guard against retries; they are kept this long. */
export const RECEIPT_DAYS = 35;

export function retentionDaysFor(plan: string, override: number | null | undefined): number {
  const base = RETENTION_DAYS[(plan in RETENTION_DAYS ? plan : "none") as PlanName];
  // A project may keep less than its plan, never more.
  return override && override > 0 ? Math.min(base, Math.floor(override)) : base;
}
