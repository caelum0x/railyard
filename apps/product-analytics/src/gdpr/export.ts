// Per-person export: one JSON document with the person, their distinct ids,
// every stored event (merged people included) and what each hook holds
// (the replay list from A3).

import type { Db } from "../db.ts";
import type { GdprHooks } from "./hooks.ts";
import { personRef } from "./person-ref.ts";

export const EXPORT_MAX_EVENTS = 100_000;

export async function exportPerson(db: Db, hooks: GdprHooks, projectId: string, distinctId: string, now: Date) {
  const { person, ref, events } = await db.asProject(projectId, async (c) => {
    const { person, ref } = await personRef(c, db, projectId, distinctId, now);
    const r = await c.query(
      `select uuid, event, distinct_id, "timestamp", properties, elements_chain, session_id
         from pa.events where project_id = $1 and (person_id = any($2::uuid[]) or distinct_id = any($3::text[]))
        order by "timestamp" limit $4`,
      [projectId, ref.personIds, ref.distinctIds, EXPORT_MAX_EVENTS],
    );
    return { person, ref, events: r.rows };
  });
  if (!person && events.length === 0) return null;
  const parts: Record<string, unknown[]> = {};
  for (const h of hooks.hooks) parts[h.name] = await h.exportPerson(ref);
  return {
    exported_at: now.toISOString(),
    project_id: projectId,
    person: person ? { id: person.id, properties: person.properties, is_identified: person.is_identified, created_at: person.created_at } : null,
    distinct_ids: ref.distinctIds,
    events,
    replays: parts.replays ?? [],
    ...Object.fromEntries(Object.entries(parts).filter(([k]) => k !== "replays")),
  };
}
