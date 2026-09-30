// Hourly rollups (pa.rollup_hourly). For each project, rolled_to is a time
// W such that every rollup row counts exactly the events of its hour with
// inserted_at < W. Each run picks a new W (now minus a settle time, so a
// writer transaction that began earlier has committed), recomputes only the
// hours touched by events inserted since the last W, and moves W. Trends
// for plain counts read rollups for whole hours and count the rest (range
// edges and events inserted since W) from events, so answers stay exact.
// A privacy deletion sets reconcile_after, and the next run rebuilds the
// project; hours older than the project's retention are dropped.

import type { Db } from "../db.ts";
import { effectiveRetention } from "../jobs/retention.ts";
import type { SiteDirectory } from "../tenancy/site-directory.ts";

export const SETTLE_MS = 5 * 60_000;
const DAY = 86_400_000;
/** Beyond this many touched hours one project is rebuilt in one statement. */
const MAX_HOURS_PER_RUN = 2000;

export interface RollupOptions {
  settleMs?: number;
  sites?: SiteDirectory;
  /** Only these projects (tests); default every project with events. */
  projects?: string[];
}

export interface RollupReport {
  projects: number;
  hours: number;
  rebuilt: string[];
}

/** Every project id with events: a loose index scan over (project_id, inserted_at). */
async function projectsWithEvents(db: Db): Promise<string[]> {
  const rows = await db.query<{ id: string }>(
    `with recursive p as (
       select (select project_id from pa.events order by project_id limit 1) as id
       union all
       select (select project_id from pa.events where project_id > p.id order by project_id limit 1) from p where p.id is not null
     )
     select id from p where id is not null`,
  );
  return rows.map((r) => r.id);
}

export async function rollupProject(db: Db, projectId: string, w: Date): Promise<{ hours: number; rebuilt: boolean }> {
  return db.asOwner(async (c) => {
    await c.query("set local statement_timeout = 600000");
    const st = (await c.query<{ rolled_to: Date; reconcile_after: Date | null }>("select rolled_to, reconcile_after from pa.rollup_state where project_id = $1 for update", [projectId])).rows[0];
    const old = st && !st.reconcile_after ? st.rolled_to : null;
    let hours: Date[] = [];
    let rebuilt = old === null;
    if (old) {
      hours = (
        await c.query<{ h: Date }>(
          `select distinct date_trunc('hour', "timestamp") as h from pa.events where project_id = $1 and inserted_at >= $2 and inserted_at < $3`,
          [projectId, old.toISOString(), w.toISOString()],
        )
      ).rows.map((r) => r.h);
      if (hours.length > MAX_HOURS_PER_RUN) rebuilt = true;
    }
    if (rebuilt) {
      await c.query("delete from pa.rollup_hourly where project_id = $1", [projectId]);
      await c.query(
        `insert into pa.rollup_hourly (project_id, event, hour, count)
         select $1, event, date_trunc('hour', "timestamp"), count(*) from pa.events
          where project_id = $1 and inserted_at < $2 group by 2, 3`,
        [projectId, w.toISOString()],
      );
    } else if (hours.length) {
      const iso = hours.map((h) => h.toISOString());
      await c.query("delete from pa.rollup_hourly where project_id = $1 and hour = any($2::timestamptz[])", [projectId, iso]);
      await c.query(
        `insert into pa.rollup_hourly (project_id, event, hour, count)
         select $1, e.event, h.h, count(*)
           from unnest($2::timestamptz[]) as h(h)
           join pa.events e on e.project_id = $1 and e."timestamp" >= h.h and e."timestamp" < h.h + interval '1 hour'
          where e.inserted_at < $3
          group by 2, 3`,
        [projectId, iso, w.toISOString()],
      );
    }
    await c.query(
      `insert into pa.rollup_state (project_id, rolled_to, reconcile_after) values ($1, $2, null)
       on conflict (project_id) do update set rolled_to = excluded.rolled_to, reconcile_after = null`,
      [projectId, w.toISOString()],
    );
    return { hours: rebuilt ? -1 : hours.length, rebuilt };
  });
}

export async function runRollups(db: Db, now: Date, opts: RollupOptions = {}): Promise<RollupReport> {
  const w = new Date(now.getTime() - (opts.settleMs ?? SETTLE_MS));
  const projects = opts.projects ?? (await projectsWithEvents(db));
  const report: RollupReport = { projects: 0, hours: 0, rebuilt: [] };
  for (const p of projects) {
    const r = await rollupProject(db, p, w);
    report.projects += 1;
    if (r.rebuilt) report.rebuilt.push(p);
    else report.hours += r.hours;
  }
  if (opts.sites) {
    for (const site of await opts.sites.all()) {
      const days = await effectiveRetention(db, site);
      await db.query("delete from pa.rollup_hourly where project_id = $1 and hour < $2", [site.project_id, new Date(now.getTime() - days * DAY).toISOString()]);
    }
  }
  return report;
}

/** The project's rollup watermark, or null when trends must count raw events (never rolled, or a rebuild is pending). */
export async function rolledTo(c: { query: (sql: string, params: unknown[]) => Promise<{ rows: { rolled_to: Date }[] }> }, projectId: string): Promise<Date | null> {
  const r = await c.query("select rolled_to from pa.rollup_state where project_id = $1 and reconcile_after is null", [projectId]);
  return r.rows[0]?.rolled_to ?? null;
}
