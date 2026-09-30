// Postgres access. Request paths run inside `asProject`: one transaction as
// the `pa_app` role (row level security on) with `pa.project_id` set, and
// every query also binds project_id. Jobs use `asOwner` and bind project_id
// themselves.

import pg from "pg";

export type Client = pg.PoolClient;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function isUuid(v: unknown): v is string {
  return typeof v === "string" && UUID.test(v);
}

export class Db {
  pool: pg.Pool;

  constructor(url: string, max = 5) {
    this.pool = new pg.Pool({ connectionString: url, max, idleTimeoutMillis: 30_000, application_name: "product-analytics" });
    this.pool.on("error", (err) => console.error(`product-analytics: idle database client error: ${err.message}`));
  }

  async tx<T>(fn: (c: Client) => Promise<T>, projectId: string | null): Promise<T> {
    if (projectId !== null && !isUuid(projectId)) throw new Error("project id must be a uuid");
    const c = await this.pool.connect();
    try {
      await c.query("begin");
      if (projectId !== null) {
        await c.query("set local role pa_app");
        await c.query("select set_config('pa.project_id', $1, true)", [projectId]);
      }
      const out = await fn(c);
      await c.query("commit");
      return out;
    } catch (err) {
      await c.query("rollback").catch(() => undefined);
      throw err;
    } finally {
      c.release();
    }
  }

  /** One transaction scoped to one project (RLS on). */
  asProject<T>(projectId: string, fn: (c: Client) => Promise<T>): Promise<T> {
    return this.tx(fn, projectId);
  }

  /** One transaction as the database owner, for jobs. Callers bind project_id. */
  asOwner<T>(fn: (c: Client) => Promise<T>): Promise<T> {
    return this.tx(fn, null);
  }

  async query<R extends pg.QueryResultRow = Record<string, unknown>>(sql: string, params: unknown[] = []): Promise<R[]> {
    const r = await this.pool.query<R>(sql, params);
    return r.rows;
  }

  close(): Promise<void> {
    return this.pool.end();
  }
}
