// Plan caps at the edge (RYP-D3, PY1.3). Pure, so the collector, the console
// and the Scribase Analytics tab say exactly the same thing.
//
// Under the cap every event is stored. At 100% of the cap the collector keeps
// counting every event (the usage meter sees them all) and stops storing new
// ones until the next UTC month. Nothing already stored is removed.
//
// The console keeps a byte-for-byte copy (apps/console/lib/caps.ts, tested).

export interface CapDecision {
  stored: number;
  capped: number;
}

/** How many of `incoming` events are stored when `used` of `limit` are used this month. */
export function capDecision(used: number, limit: number, incoming: number): CapDecision {
  const room = Math.max(0, Math.floor(limit) - Math.max(0, Math.floor(used)));
  const stored = Math.min(room, Math.max(0, incoming));
  return { stored, capped: Math.max(0, incoming) - stored };
}

/** The first day of the next UTC month, `YYYY-MM-01`. */
export function nextMonth(now: Date): string {
  const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + 1, 1));
  return d.toISOString().slice(0, 10);
}

/** What the person is told at or near the cap, or null below 80%. */
export function capMessage(used: number, limit: number, now: Date): string | null {
  if (limit <= 0) return "Events are counted but not stored: this plan stores none.";
  if (used >= limit) {
    return `This month's ${limit.toLocaleString("en-US")} stored events are used. New events are still counted, and are not stored until ${nextMonth(now)}. Nothing stored is removed.`;
  }
  if (used >= limit * 0.8) {
    return `${used.toLocaleString("en-US")} of this month's ${limit.toLocaleString("en-US")} stored events are used.`;
  }
  return null;
}
