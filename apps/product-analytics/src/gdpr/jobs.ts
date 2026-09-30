// Person deletion: requested from the console, visible as a job, done by
// the scheduler well before due_at (request + GDPR_DELETE_WITHIN_MS). The
// job removes the person's events, distinct ids, overrides and person row,
// then runs every registered hook (replays and survey answers, A3).

import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { GDPR_DELETE_WITHIN_MS } from "../plans.ts";
import type { GdprHooks } from "./hooks.ts";
import { personRef } from "./person-ref.ts";

export const MAX_ATTEMPTS = 5;

export interface GdprJob {
  id: string;
  project_id: string;
  kind: "delete";
  distinct_id: string;
  requested_by: string;
  requested_at: string;
  due_at: string;
  status: "queued" | "running" | "done" | "failed";
  started_at: string | null;
  finished_at: string | null;
  attempts: number;
  result: Record<string, number> | null;
  error: string | null;
}

const COLS = "id, project_id, kind, distinct_id, requested_by, requested_at, due_at, status, started_at, finished_at, attempts, result, error";

export async function requestDelete(db: Db, projectId: string, distinctId: string, requestedBy: string, now: Date): Promise<GdprJob> {
  return db.asProject(projectId, async (c) => {
    const open = await c.query<GdprJob>(
      `select ${COLS} from pa.gdpr_jobs where project_id = $1 and distinct_id = $2 and status in ('queued', 'running') limit 1`,
      [projectId, distinctId],
    );
    if (open.rows[0]) return open.rows[0];
    const r = await c.query<GdprJob>(
      `insert into pa.gdpr_jobs (project_id, distinct_id, requested_by, requested_at, due_at) values ($1, $2, $3, $4, $5) returning ${COLS}`,
      [projectId, distinctId, requestedBy, now.toISOString(), new Date(now.getTime() + GDPR_DELETE_WITHIN_MS).toISOString()],
    );
    return r.rows[0];
  });
}

export async function listJobs(db: Db, projectId: string, limit = 50): Promise<GdprJob[]> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<GdprJob>(`select ${COLS} from pa.gdpr_jobs where project_id = $1 order by requested_at desc limit $2`, [projectId, limit]);
    return r.rows;
  });
}

/** Deletes one person's data in one project. Returns the rows removed per part. */
export async function deletePerson(db: Db, hooks: GdprHooks, projectId: string, distinctId: string, now: Date): Promise<Record<string, number>> {
  const { counts, ref } = await db.asProject(projectId, async (c) => {
    // A background job: a person with very many events must still be deleted whole.
    await c.query("set local statement_timeout = 0");
    const { person, ref } = await personRef(c, db, projectId, distinctId, now);
    const ev = await c.query("delete from pa.events where project_id = $1 and (person_id = any($2::uuid[]) or distinct_id = any($3::text[]))", [projectId, ref.personIds, ref.distinctIds]);
    const ov = await c.query("delete from pa.person_overrides where project_id = $1 and (old_person_id = any($2::uuid[]) or new_person_id = any($2::uuid[]))", [projectId, ref.personIds]);
    const ids = await c.query("delete from pa.person_distinct_ids where project_id = $1 and distinct_id = any($2::text[])", [projectId, ref.distinctIds]);
    const p = person ? await c.query("delete from pa.persons where project_id = $1 and id = $2", [projectId, person.id]) : { rowCount: 0 };
    return { counts: { events: ev.rowCount ?? 0, overrides: ov.rowCount ?? 0, distinct_ids: ids.rowCount ?? 0, persons: p.rowCount ?? 0 }, ref };
  });
  const out: Record<string, number> = { ...counts };
  for (const h of hooks.hooks) out[h.name] = await h.deletePerson(ref);
  return out;
}

/** Runs queued jobs (oldest due first). Returns how many finished. */
export async function runDueJobs(db: Db, hooks: GdprHooks, log: Logger, now: Date, batch = 20): Promise<number> {
  const claimed = await db.asOwner(async (c) => {
    const r = await c.query<GdprJob>(
      `update pa.gdpr_jobs set status = 'running', started_at = $1, attempts = attempts + 1
        where id in (select id from pa.gdpr_jobs where status = 'queued' order by due_at limit $2 for update skip locked)
        returning ${COLS}`,
      [now.toISOString(), batch],
    );
    return r.rows;
  });
  let done = 0;
  for (const job of claimed) {
    try {
      const result = await deletePerson(db, hooks, job.project_id, job.distinct_id, now);
      await db.query("update pa.gdpr_jobs set status = 'done', finished_at = $3, result = $4, error = null where id = $1 and project_id = $2", [job.id, job.project_id, now.toISOString(), JSON.stringify(result)]);
      done += 1;
    } catch (err) {
      const failed = job.attempts >= MAX_ATTEMPTS;
      await db.query("update pa.gdpr_jobs set status = $3, error = $4 where id = $1 and project_id = $2", [job.id, job.project_id, failed ? "failed" : "queued", (err as Error).message.slice(0, 500)]);
      log.error("privacy deletion failed", { job: job.id, project_id: job.project_id, attempt: job.attempts, error: (err as Error).message });
    }
  }
  return done;
}

/** Jobs left in `running` by a stopped process go back to the queue. */
export async function requeueStale(db: Db, now: Date, staleMs = 15 * 60_000): Promise<number> {
  const r = await db.query<{ id: string }>(
    "update pa.gdpr_jobs set status = 'queued' where status = 'running' and started_at < $1 returning id",
    [new Date(now.getTime() - staleMs).toISOString()],
  );
  return r.length;
}
