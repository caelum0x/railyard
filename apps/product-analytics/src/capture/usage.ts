// Stored events this UTC month per org, for the cap decision: the database
// count (refreshed at most every REFRESH_MS, and after every flush of the
// org) plus the org's rows still waiting in the insert buffer.

import type { Db } from "../db.ts";
import { monthOf } from "./writer.ts";

export const REFRESH_MS = 30_000;

interface Entry {
  month: string;
  dbStored: number;
  at: number;
}

export class UsageCounter {
  db: Db;
  now: () => Date;
  pending: (orgId: string) => number;
  entries = new Map<string, Entry>();

  constructor(db: Db, pending: (orgId: string) => number, now: () => Date = () => new Date()) {
    this.db = db;
    this.pending = pending;
    this.now = now;
  }

  async used(orgId: string): Promise<number> {
    const now = this.now();
    const month = monthOf(now);
    const cur = this.entries.get(orgId);
    if (cur && cur.month === month && now.getTime() - cur.at < REFRESH_MS) return cur.dbStored + this.pending(orgId);
    const rows = await this.db.query<{ stored: string }>(
      "select coalesce(sum(stored), 0)::bigint as stored from pa.usage_monthly where org_id = $1 and month = $2",
      [orgId, month],
    );
    this.entries.set(orgId, { month, dbStored: Number(rows[0]?.stored ?? 0), at: now.getTime() });
    return Number(rows[0]?.stored ?? 0) + this.pending(orgId);
  }

  /** Called by the writer after a flush stored `n` rows of an org: the next read sees them in the database count. */
  stored(orgId: string, month: string, n: number): void {
    const cur = this.entries.get(orgId);
    if (cur && cur.month === month) this.entries.set(orgId, { ...cur, dbStored: cur.dbStored + n });
  }
}
