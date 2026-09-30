import type { Client, Db } from "../db.ts";
import { personIdFor } from "../persons/ids.ts";
import { findPerson, type Person } from "../persons/service.ts";
import type { PersonRef } from "./hooks.ts";

/** Everything that identifies one person in a project, from any of its distinct ids. */
export async function personRef(c: Client, db: Db, projectId: string, distinctId: string, now: Date): Promise<{ person: Person | null; ref: PersonRef }> {
  const person = await findPerson(c, projectId, distinctId);
  let distinctIds = [distinctId];
  const personIds = new Set<string>();
  if (person) {
    personIds.add(person.id);
    const ids = await c.query<{ distinct_id: string }>("select distinct_id from pa.person_distinct_ids where project_id = $1 and person_id = $2 order by distinct_id", [projectId, person.id]);
    distinctIds = ids.rows.map((r) => r.distinct_id);
    const olds = await c.query<{ old_person_id: string }>("select old_person_id from pa.person_overrides where project_id = $1 and new_person_id = $2", [projectId, person.id]);
    for (const r of olds.rows) personIds.add(r.old_person_id);
  }
  for (const d of distinctIds) personIds.add(personIdFor(projectId, d));
  return { person, ref: { db, projectId, personIds: [...personIds], distinctIds, now } };
}
