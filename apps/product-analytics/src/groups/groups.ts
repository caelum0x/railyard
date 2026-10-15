// Group analytics, following PostHog's group semantics (MIT, commit
// cbb1e9253027): a project has up to five group types, numbered 0-4 in the
// order they are first seen. `$groupidentify` sets group properties
// ($group_set); an event's properties.$groups {type: key} fill the event's
// "$group_0".."$group_4" columns. Runs inside Db.asProject.

import type { Client } from "../db.ts";
import { applyOps, type PropertyOps } from "../persons/properties.ts";

export const MAX_GROUP_TYPES = 5;

type Obj = Record<string, unknown>;

/** The index of a group type, created on first use; null once five types exist. */
export async function groupTypeIndex(c: Client, projectId: string, groupType: string, cache: Map<string, number | null>): Promise<number | null> {
  const k = `${projectId}:${groupType}`;
  if (cache.has(k)) return cache.get(k)!;
  const found = await c.query<{ group_type_index: number }>("select group_type_index from pa.group_types where project_id = $1 and group_type = $2", [projectId, groupType]);
  if (found.rows[0]) {
    cache.set(k, found.rows[0].group_type_index);
    return found.rows[0].group_type_index;
  }
  const used = await c.query<{ group_type_index: number }>("select group_type_index from pa.group_types where project_id = $1", [projectId]);
  const taken = new Set(used.rows.map((r) => r.group_type_index));
  const free = [0, 1, 2, 3, 4].find((i) => !taken.has(i));
  if (free === undefined) {
    cache.set(k, null);
    return null;
  }
  await c.query("insert into pa.group_types (project_id, group_type, group_type_index) values ($1, $2, $3) on conflict do nothing", [projectId, groupType, free]);
  const again = await c.query<{ group_type_index: number }>("select group_type_index from pa.group_types where project_id = $1 and group_type = $2", [projectId, groupType]);
  const idx = again.rows[0]?.group_type_index ?? null;
  cache.set(k, idx);
  return idx;
}

/** Applies `ops` (set/set_once/unset plus Mixpanel's add/append/union/remove) to a group's properties, creating the group when new. Same shape as person property updates (../persons/properties.ts). */
export async function upsertGroup(c: Client, projectId: string, index: number, key: string, ops: PropertyOps, at: Date): Promise<void> {
  const current = await c.query<{ properties: Obj }>("select properties from pa.groups where project_id = $1 and group_type_index = $2 and group_key = $3", [projectId, index, key]);
  const existing = current.rows[0]?.properties ?? {};
  const next = applyOps(existing, ops) ?? existing;
  await c.query(
    `insert into pa.groups (project_id, group_type_index, group_key, properties, created_at, updated_at)
     values ($1, $2, $3, $4, $5, $5)
     on conflict (project_id, group_type_index, group_key)
     do update set properties = excluded.properties, updated_at = excluded.updated_at, version = pa.groups.version + 1`,
    [projectId, index, key, JSON.stringify(next), at.toISOString()],
  );
}

/** `set`-only ops, for callers (event-based $groupidentify) that don't use set_once/unset/add/append/union/remove. */
export function setOps(set: Obj): PropertyOps {
  return { set, setOnce: {}, unset: [], add: {}, append: {}, union: {}, remove: {} };
}

/** The five group columns of an event. */
export async function groupColumns(c: Client, projectId: string, groups: Record<string, string>, cache: Map<string, number | null>): Promise<(string | null)[]> {
  const cols: (string | null)[] = [null, null, null, null, null];
  for (const [type, key] of Object.entries(groups)) {
    const idx = await groupTypeIndex(c, projectId, type, cache);
    if (idx !== null) cols[idx] = key;
  }
  return cols;
}
