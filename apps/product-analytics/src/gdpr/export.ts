// Per-person export: one JSON document with the person, their distinct ids,
// every stored event (merged people included) and what each hook holds
// (the replay list from A3). Events are read in pages and streamed, so a
// person with any number of events is exported whole without holding them
// all in the service's memory.

import type { Db } from "../db.ts";
import type { GdprHooks } from "./hooks.ts";
import { personRef } from "./person-ref.ts";

/** Events read per page while streaming. */
export const EXPORT_PAGE_SIZE = 1000;

interface Cursor {
  ts: string;
  uuid: string;
}

/**
 * The export as a stream of JSON text pieces, or null when the id has no
 * person and no events in this project. The pieces join to one document.
 */
export async function openPersonExport(
  db: Db,
  hooks: GdprHooks,
  projectId: string,
  distinctId: string,
  now: Date,
  pageSize = EXPORT_PAGE_SIZE,
): Promise<AsyncIterable<string> | null> {
  const { person, ref, any } = await db.asProject(projectId, async (c) => {
    const { person, ref } = await personRef(c, db, projectId, distinctId, now);
    const r = await c.query("select 1 from pa.events where project_id = $1 and (person_id = any($2::uuid[]) or distinct_id = any($3::text[])) limit 1", [projectId, ref.personIds, ref.distinctIds]);
    return { person, ref, any: r.rows.length > 0 };
  });
  if (!person && !any) return null;
  const parts: Record<string, unknown[]> = {};
  for (const h of hooks.hooks) parts[h.name] = await h.exportPerson(ref);
  const head = {
    exported_at: now.toISOString(),
    project_id: projectId,
    person: person ? { id: person.id, properties: person.properties, is_identified: person.is_identified, created_at: person.created_at } : null,
    distinct_ids: ref.distinctIds,
  };
  const tail = { replays: parts.replays ?? [], ...Object.fromEntries(Object.entries(parts).filter(([k]) => k !== "replays")) };

  async function* pieces(): AsyncGenerator<string> {
    yield `${JSON.stringify(head).slice(0, -1)},"events":[`;
    let cursor: Cursor | null = null;
    let first = true;
    for (;;) {
      const after: Cursor | null = cursor;
      const rows: Record<string, unknown>[] = await db.asProject(projectId, async (c): Promise<Record<string, unknown>[]> =>
        (
          await c.query(
            `select uuid, event, distinct_id, "timestamp", properties, elements_chain, session_id, "timestamp"::text as ts_text
               from pa.events where project_id = $1 and (person_id = any($2::uuid[]) or distinct_id = any($3::text[]))
                and ($4::timestamptz is null or ("timestamp", uuid) > ($4::timestamptz, $5::uuid))
              order by "timestamp", uuid limit $6`,
            [projectId, ref.personIds, ref.distinctIds, after?.ts ?? null, after?.uuid ?? null, pageSize],
          )
        ).rows,
      );
      if (rows.length === 0) break;
      const out: string[] = [];
      for (const r of rows) {
        const { ts_text: _ts, ...ev } = r;
        out.push(JSON.stringify(ev));
      }
      yield (first ? "" : ",") + out.join(",");
      first = false;
      const last: Record<string, unknown> = rows[rows.length - 1];
      cursor = { ts: String(last.ts_text), uuid: String(last.uuid) };
      if (rows.length < pageSize) break;
    }
    yield `],${JSON.stringify(tail).slice(1)}`;
  }
  return pieces();
}

/** The whole export as one object (small exports and tests). */
export async function exportPerson(db: Db, hooks: GdprHooks, projectId: string, distinctId: string, now: Date) {
  const stream = await openPersonExport(db, hooks, projectId, distinctId, now);
  if (!stream) return null;
  let text = "";
  for await (const piece of stream) text += piece;
  return JSON.parse(text) as Record<string, unknown>;
}
