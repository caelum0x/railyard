// Identify, alias and merge, ported from PostHog's PersonMergeService
// semantics (MIT, commit cbb1e9253027,
// nodejs/src/ingestion/common/persons/person-merge-service.ts):
//   - `$create_alias` / `$merge_dangerously` with properties.alias merge the
//     alias's person into the event's distinct_id person;
//   - `$identify` with $anon_distinct_id merges the anonymous person into the
//     distinct_id person;
//   - if neither id has a person, one person is created with both ids;
//   - if only one has a person, the other id is added to it;
//   - if both have different people, the merge is refused when the source
//     person is already identified (unless $merge_dangerously); otherwise the
//     source person is merged into the target: target properties win, the
//     source's ids move, and an override row maps the old person id to the
//     new one so earlier events count for the merged person.
// Every function runs inside Db.asProject (row level security on) and binds
// project_id.

import type { Client } from "../db.ts";
import { isDistinctIdUnmergeable, personIdFor } from "./ids.ts";
import { applyOps, eventOps, type PropertyOps } from "./properties.ts";

type Obj = Record<string, unknown>;

export interface Person {
  id: string;
  properties: Obj;
  is_identified: boolean;
  created_at: string;
  version: number;
}

export type MergeOutcome = "none" | "merged" | "linked" | "created" | "refused_identified" | "illegal_id";

export interface PersonEvent {
  event: string;
  distinct_id: string;
  timestamp: Date;
  properties: Obj;
  set: Obj | null;
  set_once: Obj | null;
  unset: string[] | null;
}

export interface PersonResult {
  person_id: string;
  merge: MergeOutcome;
}

const EMPTY_OPS: PropertyOps = { set: {}, setOnce: {}, unset: [], add: {}, append: {}, union: {}, remove: {} };

export async function findPerson(c: Client, projectId: string, distinctId: string): Promise<Person | null> {
  const r = await c.query<Person>(
    `select p.id, p.properties, p.is_identified, p.created_at, p.version
       from pa.person_distinct_ids d join pa.persons p on p.id = d.person_id and p.project_id = d.project_id
      where d.project_id = $1 and d.distinct_id = $2`,
    [projectId, distinctId],
  );
  return r.rows[0] ?? null;
}

/** Maps the personless id of `distinctId` to `personId`, so events stored before a person existed resolve to it. */
async function linkPersonless(c: Client, projectId: string, distinctId: string, personId: string): Promise<void> {
  const old = personIdFor(projectId, distinctId);
  if (old === personId) return;
  const live = await c.query("select 1 from pa.persons where project_id = $1 and id = $2", [projectId, old]);
  if (live.rowCount) return;
  await c.query(
    `insert into pa.person_overrides (project_id, old_person_id, new_person_id) values ($1, $2, $3)
     on conflict (project_id, old_person_id) do update set new_person_id = excluded.new_person_id`,
    [projectId, old, personId],
  );
}

async function addDistinctId(c: Client, projectId: string, distinctId: string, personId: string): Promise<void> {
  await c.query("insert into pa.person_distinct_ids (project_id, distinct_id, person_id) values ($1, $2, $3)", [projectId, distinctId, personId]);
  await linkPersonless(c, projectId, distinctId, personId);
}

async function createPerson(c: Client, projectId: string, distinctIds: string[], props: Obj, identified: boolean, at: Date): Promise<Person> {
  const id = personIdFor(projectId, distinctIds[0]);
  const r = await c.query<Person>(
    `insert into pa.persons (id, project_id, properties, is_identified, created_at) values ($1, $2, $3, $4, $5)
     returning id, properties, is_identified, created_at, version`,
    [id, projectId, JSON.stringify(props), identified, at.toISOString()],
  );
  for (const d of distinctIds) await addDistinctId(c, projectId, d, id);
  return r.rows[0];
}

async function savePerson(c: Client, projectId: string, p: Person, props: Obj | null, identified: boolean): Promise<Person> {
  if (props === null && p.is_identified === identified) return p;
  const r = await c.query<Person>(
    `update pa.persons set properties = $3, is_identified = $4, version = version + 1
      where project_id = $1 and id = $2 returning id, properties, is_identified, created_at, version`,
    [projectId, p.id, JSON.stringify(props ?? p.properties), identified],
  );
  return r.rows[0];
}

/** Moves everything of `src` onto `dst` and removes `src`; returns the updated target. */
async function mergePeople(c: Client, projectId: string, src: Person, dst: Person, ops: PropertyOps): Promise<Person> {
  const merged = applyOps({ ...src.properties, ...dst.properties }, ops) ?? { ...src.properties, ...dst.properties };
  const createdAt = Date.parse(src.created_at) < Date.parse(dst.created_at) ? src.created_at : dst.created_at;
  await c.query("update pa.person_distinct_ids set person_id = $3, version = version + 1 where project_id = $1 and person_id = $2", [projectId, src.id, dst.id]);
  await c.query("update pa.person_overrides set new_person_id = $3 where project_id = $1 and new_person_id = $2", [projectId, src.id, dst.id]);
  await c.query("delete from pa.person_overrides where project_id = $1 and old_person_id = new_person_id", [projectId]);
  await c.query(
    `insert into pa.person_overrides (project_id, old_person_id, new_person_id) values ($1, $2, $3)
     on conflict (project_id, old_person_id) do update set new_person_id = excluded.new_person_id`,
    [projectId, src.id, dst.id],
  );
  await c.query("delete from pa.persons where project_id = $1 and id = $2", [projectId, src.id]);
  const r = await c.query<Person>(
    `update pa.persons set properties = $3, is_identified = true, created_at = $4, version = version + 1
      where project_id = $1 and id = $2 returning id, properties, is_identified, created_at, version`,
    [projectId, dst.id, JSON.stringify(merged), createdAt],
  );
  return r.rows[0];
}

export async function merge(
  c: Client,
  projectId: string,
  otherId: string,
  intoId: string,
  ops: PropertyOps,
  at: Date,
  allowIdentifiedSources: boolean,
): Promise<{ person: Person | null; outcome: MergeOutcome }> {
  if (otherId === intoId) return { person: null, outcome: "none" };
  if (isDistinctIdUnmergeable(intoId) || isDistinctIdUnmergeable(otherId)) return { person: null, outcome: "illegal_id" };
  const other = await findPerson(c, projectId, otherId);
  const into = await findPerson(c, projectId, intoId);
  if (other && into) {
    if (other.id === into.id) {
      return { person: await savePerson(c, projectId, into, applyOps(into.properties, ops), true), outcome: "none" };
    }
    if (other.is_identified && !allowIdentifiedSources) {
      // Upstream: "refused to merge an already identified user via an $identify or $create_alias call".
      return { person: await savePerson(c, projectId, into, applyOps(into.properties, ops), true), outcome: "refused_identified" };
    }
    return { person: await mergePeople(c, projectId, other, into, ops), outcome: "merged" };
  }
  if (other && !into) {
    await addDistinctId(c, projectId, intoId, other.id);
    return { person: await savePerson(c, projectId, other, applyOps(other.properties, ops), true), outcome: "linked" };
  }
  if (!other && into) {
    await addDistinctId(c, projectId, otherId, into.id);
    return { person: await savePerson(c, projectId, into, applyOps(into.properties, ops), true), outcome: "linked" };
  }
  const props = applyOps({}, ops) ?? {};
  return { person: await createPerson(c, projectId, [intoId, otherId], props, true, at), outcome: "created" };
}

/** Resolves (and updates) the person of one event; returns the person id to store on the event. */
export async function processPerson(c: Client, projectId: string, ev: PersonEvent): Promise<PersonResult> {
  const props = ev.properties;
  const personless = props.$process_person_profile === false;
  const ops = personless ? EMPTY_OPS : eventOps(ev);
  const alias = typeof props.alias === "string" || typeof props.alias === "number" ? String(props.alias) : null;
  const anon = "$anon_distinct_id" in props && props.$anon_distinct_id !== null && props.$anon_distinct_id !== undefined ? String(props.$anon_distinct_id) : null;

  let outcome: MergeOutcome = "none";
  if ((ev.event === "$create_alias" || ev.event === "$merge_dangerously") && alias) {
    const r = await merge(c, projectId, alias, ev.distinct_id, ops, ev.timestamp, ev.event === "$merge_dangerously");
    outcome = r.outcome;
    if (r.person) return { person_id: r.person.id, merge: outcome };
  } else if (ev.event === "$identify" && anon) {
    const r = await merge(c, projectId, anon, ev.distinct_id, ops, ev.timestamp, false);
    outcome = r.outcome;
    if (r.person) return { person_id: r.person.id, merge: outcome };
  }

  const existing = await findPerson(c, projectId, ev.distinct_id);
  if (personless && ev.event !== "$identify") {
    return { person_id: existing?.id ?? personIdFor(projectId, ev.distinct_id), merge: outcome };
  }
  const identify = ev.event === "$identify" && !isDistinctIdUnmergeable(ev.distinct_id);
  if (!existing) {
    const p = await createPerson(c, projectId, [ev.distinct_id], applyOps({}, ops) ?? {}, identify, ev.timestamp);
    return { person_id: p.id, merge: outcome };
  }
  const p = await savePerson(c, projectId, existing, applyOps(existing.properties, ops), existing.is_identified || identify);
  return { person_id: p.id, merge: outcome };
}

/** /engage/ style updates without an event. */
export async function updatePersonProperties(c: Client, projectId: string, distinctId: string, ops: PropertyOps, at: Date): Promise<string> {
  const existing = await findPerson(c, projectId, distinctId);
  if (!existing) return (await createPerson(c, projectId, [distinctId], applyOps({}, ops) ?? {}, false, at)).id;
  return (await savePerson(c, projectId, existing, applyOps(existing.properties, ops), existing.is_identified)).id;
}
