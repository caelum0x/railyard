// Retention: whole monthly partitions are dropped once they end before the
// longest plan's window (cheap, no row scans); then every project is trimmed
// nightly in batches to its own window (plan or shorter setting), which only
// touches the few rows in partitions that straddle a cutoff. Nothing inside a
// project's window is ever removed. Receipts are only kept RECEIPT_DAYS.

import type { Db } from "../db.ts";
import { LONGEST_RETENTION_DAYS, RECEIPT_DAYS, retentionDaysFor } from "../plans.ts";
import { getSettings } from "../tenancy/settings.ts";
import type { Site, SiteDirectory } from "../tenancy/site-directory.ts";

const DAY_MS = 86_400_000;
export const TRIM_BATCH = 10_000;

/** Creates this month's and the next two months' partitions. */
export async function ensurePartitions(db: Db, now: Date): Promise<string[]> {
  const out: string[] = [];
  for (let i = 0; i <= 2; i += 1) {
    const d = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth() + i, 1));
    const r = await db.query<{ n: string }>("select pa.ensure_month($1::date) as n", [d.toISOString().slice(0, 10)]);
    out.push(r[0].n);
  }
  return out;
}

/** Drops partitions whose whole month is older than the longest plan's window. */
export async function dropExpiredPartitions(db: Db, now: Date): Promise<string[]> {
  const cutoff = new Date(now.getTime() - LONGEST_RETENTION_DAYS * DAY_MS);
  const r = await db.query<{ n: string }>("select pa.drop_months_before($1) as n", [cutoff.toISOString()]);
  return r.map((x) => x.n);
}

/** Deletes one project's events older than its window, in batches. Returns rows removed. */
export async function trimProject(db: Db, projectId: string, days: number, now: Date): Promise<number> {
  const cutoff = new Date(now.getTime() - days * DAY_MS).toISOString();
  let total = 0;
  for (;;) {
    const n = await db.asProject(projectId, async (c) => {
      const r = await c.query(
        `delete from pa.events where project_id = $1 and "timestamp" < $2
           and uuid in (select uuid from pa.events where project_id = $1 and "timestamp" < $2 limit $3)`,
        [projectId, cutoff, TRIM_BATCH],
      );
      return r.rowCount ?? 0;
    });
    total += n;
    if (n < TRIM_BATCH) return total;
  }
}

export async function effectiveRetention(db: Db, site: Site): Promise<number> {
  const s = await getSettings(db, site.project_id);
  return retentionDaysFor(site.plan, s.retention_days ?? null);
}

export async function trimProjects(db: Db, sites: SiteDirectory, now: Date): Promise<Record<string, number>> {
  const out: Record<string, number> = {};
  for (const site of await sites.all()) {
    const days = await effectiveRetention(db, site);
    const n = await trimProject(db, site.project_id, days, now);
    if (n) out[site.project_id] = n;
  }
  return out;
}

export async function purgeReceipts(db: Db, now: Date): Promise<number> {
  const cutoff = new Date(now.getTime() - RECEIPT_DAYS * DAY_MS).toISOString();
  let total = 0;
  for (;;) {
    const r = await db.query<{ n: string }>(
      `with gone as (delete from pa.ingest_receipts where ctid in (select ctid from pa.ingest_receipts where received_at < $1 limit $2) returning 1)
       select count(*)::bigint as n from gone`,
      [cutoff, TRIM_BATCH],
    );
    const n = Number(r[0].n);
    total += n;
    if (n < TRIM_BATCH) return total;
  }
}

export async function runRetention(db: Db, sites: SiteDirectory, now: Date) {
  await ensurePartitions(db, now);
  const dropped = await dropExpiredPartitions(db, now);
  const trimmed = await trimProjects(db, sites, now);
  const receipts = await purgeReceipts(db, now);
  return { dropped, trimmed, receipts };
}
