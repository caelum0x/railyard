// Actions: named sets of match steps (OR across steps), used as a series,
// a funnel step or a retention event. Stored per project; the matching SQL
// is in filters.ts (stepSql).

import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { QueryError, type ActionDef } from "./types.ts";
import { parseActionSteps } from "./validate.ts";

export interface ActionRow extends ActionDef {
  created_at: string;
  updated_at: string;
}

function checkName(v: unknown): string {
  if (typeof v !== "string" || v.trim().length < 1 || v.trim().length > 200) throw new QueryError("Give it a name of 1 to 200 characters.");
  return v.trim();
}

const duplicate = (err: unknown) => (err as { code?: string }).code === "23505";

export async function listActions(db: Db, projectId: string): Promise<ActionRow[]> {
  return db.asProject(projectId, async (c) => (await c.query<ActionRow>("select id, name, steps, created_at, updated_at from pa.actions where project_id = $1 order by name", [projectId])).rows);
}

export async function createAction(db: Db, projectId: string, input: { name: unknown; steps: unknown }): Promise<ActionRow> {
  const name = checkName(input.name);
  const steps = parseActionSteps(input.steps);
  try {
    return await db.asProject(projectId, async (c) =>
      (await c.query<ActionRow>("insert into pa.actions (project_id, name, steps) values ($1, $2, $3) returning id, name, steps, created_at, updated_at", [projectId, name, JSON.stringify(steps)])).rows[0],
    );
  } catch (err) {
    if (duplicate(err)) throw new QueryError("An action with that name already exists.");
    throw err;
  }
}

export async function updateAction(db: Db, projectId: string, id: string, input: { name: unknown; steps: unknown }): Promise<ActionRow | null> {
  if (!isUuid(id)) return null;
  const name = checkName(input.name);
  const steps = parseActionSteps(input.steps);
  try {
    return await db.asProject(projectId, async (c) => {
      const r = await c.query<ActionRow>(
        "update pa.actions set name = $3, steps = $4, updated_at = now() where project_id = $1 and id = $2 returning id, name, steps, created_at, updated_at",
        [projectId, id, name, JSON.stringify(steps)],
      );
      if (r.rows[0]) await bumpVersion(c, projectId);
      return r.rows[0] ?? null;
    });
  } catch (err) {
    if (duplicate(err)) throw new QueryError("An action with that name already exists.");
    throw err;
  }
}

export async function deleteAction(db: Db, projectId: string, id: string): Promise<boolean> {
  if (!isUuid(id)) return false;
  return db.asProject(projectId, async (c) => ((await c.query("delete from pa.actions where project_id = $1 and id = $2", [projectId, id])).rowCount ?? 0) > 0);
}

/** Marks every cached answer of the project stale (an action or cohort changed, or a person was deleted). */
export async function bumpVersion(c: { query: (sql: string, params: unknown[]) => Promise<unknown> }, projectId: string): Promise<void> {
  await c.query(
    "insert into pa.insight_versions (project_id, version) values ($1, 1) on conflict (project_id) do update set version = pa.insight_versions.version + 1",
    [projectId],
  );
}
