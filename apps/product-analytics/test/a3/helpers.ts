// A3 test helpers on top of A1's harness (scratch Postgres, service on a
// random port): console tokens, console API calls and direct event rows for
// fixtures (inserted as the owner, the way A2's fixtures load).

import { randomUUID } from "node:crypto";
import pg from "pg";
import { personIdFor } from "../../src/persons/ids.ts";
import { signConsoleToken, type ConsoleClaims } from "../../src/tenancy/console-token.ts";
import type { Site } from "../../src/tenancy/site-directory.ts";
import type { Running, ScratchPg } from "../a1/helpers.ts";

// Background jobs are driven by hand in tests.
process.env.PA_INSIGHTS_JOBS = "off";
process.env.PA_A3_JOBS = "off";

export const SECRET = "a3".repeat(24);

export async function token(site: Site, role: ConsoleClaims["role"] = "owner", now = new Date()): Promise<string> {
  return signConsoleToken(SECRET, { org_id: site.org_id, app_ids: [site.project_id], role }, now);
}

export interface ApiAnswer<T = Record<string, unknown>> {
  status: number;
  body: T;
  headers: Headers;
}

/** A console API call as `role` of the site's org. */
export async function api<T = Record<string, any>>(run: Running, site: Site, path: string, init: { method?: string; body?: unknown; role?: ConsoleClaims["role"]; now?: Date } = {}): Promise<ApiAnswer<T>> {
  const res = await fetch(`${run.base}/v1/projects/${site.project_id}${path}`, {
    method: init.method ?? (init.body === undefined ? "GET" : "POST"),
    headers: { authorization: `Bearer ${await token(site, init.role ?? "owner", init.now)}`, "content-type": "application/json" },
    body: init.body === undefined ? undefined : JSON.stringify(init.body),
  });
  const text = await res.text();
  let body: unknown = text;
  try {
    body = JSON.parse(text);
  } catch {
    /* not JSON */
  }
  return { status: res.status, body: body as T, headers: res.headers };
}

export interface Row {
  event: string;
  distinct_id: string;
  timestamp: Date;
  properties?: Record<string, unknown>;
  session_id?: string | null;
  person_id?: string;
}

/** Inserts event rows directly (the partitions are created first). Returns their uuids. */
export async function insertEvents(scratch: ScratchPg, projectId: string, rows: Row[]): Promise<string[]> {
  const c = new pg.Client({ connectionString: scratch.url });
  await c.connect();
  try {
    const months = [...new Set(rows.map((r) => r.timestamp.toISOString().slice(0, 7)))];
    for (const m of months) await c.query("select pa.ensure_month($1::date)", [`${m}-01`]);
    const ids: string[] = [];
    for (let i = 0; i < rows.length; i += 500) {
      const part = rows.slice(i, i + 500);
      const values: unknown[] = [];
      const tuples = part.map((r, j) => {
        const id = randomUUID();
        ids.push(id);
        values.push(id, projectId, r.event, r.distinct_id, r.person_id ?? personIdFor(projectId, r.distinct_id), r.timestamp.toISOString(), JSON.stringify(r.properties ?? {}), r.session_id ?? null);
        const b = j * 8;
        return `($${b + 1}, $${b + 2}, $${b + 3}, $${b + 4}, $${b + 5}, $${b + 6}, $${b + 7}::jsonb, $${b + 8})`;
      });
      await c.query(`insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp", properties, session_id) values ${tuples.join(", ")}`, values);
    }
    return ids;
  } finally {
    await c.end();
  }
}

export const waitFor = async (fn: () => boolean | Promise<boolean>, ms = 15_000, step = 100): Promise<boolean> => {
  const until = Date.now() + ms;
  while (Date.now() < until) {
    if (await fn()) return true;
    await new Promise((r) => setTimeout(r, step));
  }
  return false;
};
