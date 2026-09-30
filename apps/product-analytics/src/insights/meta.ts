// What the insight builder offers to pick from: the project's event names
// (last 30 days, by volume), event and person property keys (sampled), group
// types, actions and cohorts.

import type { Db } from "../db.ts";

export interface ProjectMeta {
  events: { event: string; count: number }[];
  event_properties: string[];
  person_properties: string[];
  group_types: { index: number; name: string }[];
  actions: { id: string; name: string }[];
  cohorts: { id: string; name: string; people_count: number }[];
}

export async function projectMeta(db: Db, projectId: string, now: Date): Promise<ProjectMeta> {
  return db.asProject(projectId, async (c) => {
    await c.query("set local statement_timeout = 10000");
    const since = new Date(now.getTime() - 30 * 86_400_000).toISOString();
    const events = await c.query<{ event: string; count: string }>(
      `select event, count(*) as count from pa.events where project_id = $1 and "timestamp" >= $2 group by 1 order by 2 desc, 1 limit 200`,
      [projectId, since],
    );
    const eprops = await c.query<{ k: string }>(
      `select distinct k from (select properties from pa.events where project_id = $1 and "timestamp" >= $2 order by "timestamp" desc limit 2000) s,
              jsonb_object_keys(s.properties) as k order by k limit 300`,
      [projectId, since],
    );
    const pprops = await c.query<{ k: string }>(
      `select distinct k from (select properties from pa.persons where project_id = $1 order by created_at desc limit 2000) s, jsonb_object_keys(s.properties) as k order by k limit 300`,
      [projectId],
    );
    const groups = await c.query<{ index: number; name: string }>("select group_type_index as index, group_type as name from pa.group_types where project_id = $1 order by 1", [projectId]);
    const actions = await c.query<{ id: string; name: string }>("select id, name from pa.actions where project_id = $1 order by name", [projectId]);
    const cohorts = await c.query<{ id: string; name: string; people_count: number }>("select id, name, people_count::int as people_count from pa.cohorts where project_id = $1 order by name", [projectId]);
    return {
      events: events.rows.map((r) => ({ event: r.event, count: Number(r.count) })),
      event_properties: eprops.rows.map((r) => r.k),
      person_properties: pprops.rows.map((r) => r.k),
      group_types: groups.rows,
      actions: actions.rows,
      cohorts: cohorts.rows,
    };
  });
}
