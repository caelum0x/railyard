// Cohorts. Static cohorts come from a CSV of distinct ids or emails;
// behavioural ones ("did X at least N times in D days", "did not do Y",
// plus person properties) are recomputed into pa.cohort_people, nightly and
// on demand. Members are person ids; insight filters resolve them through
// merges (filters.ts cohortMembers).

import type { Client, Db } from "../db.ts";
import { isUuid } from "../db.ts";
import type { GdprHook, PersonRef } from "../gdpr/hooks.ts";
import { bumpVersion } from "./actions.ts";
import { compareProperty, Joins, seriesSql } from "./filters.ts";
import { loadActions } from "./runner.ts";
import { checkKey, Params, PERSON, PERSON_JOIN } from "./sql.ts";
import { QueryError, type BuildContext, type PropertyOperator, type PropertyValue, type Series } from "./types.ts";
import { parseFilters } from "./validate.ts";

export type Criterion =
  | { type: "behaviour"; event?: string | null; action_id?: string; operator: "gte" | "lte"; count: number; days: number }
  | { type: "person"; key: string; operator: PropertyOperator; value?: PropertyValue };

export interface BehaviouralDefinition {
  match: "all" | "any";
  criteria: Criterion[];
}

export interface CohortRow {
  id: string;
  name: string;
  kind: "static" | "behavioural";
  definition: BehaviouralDefinition | Record<string, never>;
  version: number;
  people_count: number;
  computed_at: string | null;
  error: string | null;
  created_at: string;
}

export const MAX_CSV_BYTES = 5 * 1024 * 1024;
export const MAX_CSV_ROWS = 100_000;
const COLS = "id, name, kind, definition, version, people_count::int as people_count, computed_at, error, created_at";

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

export function parseDefinition(v: unknown): BehaviouralDefinition {
  if (!isObj(v) || !Array.isArray(v.criteria) || v.criteria.length < 1 || v.criteria.length > 10) throw new QueryError("A segment needs 1 to 10 conditions.");
  const match = v.match === "any" ? "any" : "all";
  const criteria = v.criteria.map((c): Criterion => {
    if (!isObj(c)) throw new QueryError("Each condition must be an object.");
    if (c.type === "person") {
      const [pf] = parseFilters([{ ...c, type: "person" }]);
      if (pf.type !== "person") throw new QueryError("Pick a person property.");
      return { type: "person", key: pf.key, operator: pf.operator, value: pf.value };
    }
    if (c.type !== "behaviour") throw new QueryError("A condition is about what people did or about their properties.");
    const operator = c.operator === "lte" ? "lte" : "gte";
    const count = c.count;
    const days = c.days;
    if (typeof count !== "number" || !Number.isInteger(count) || count < 0 || count > 100_000) throw new QueryError("Set how many times, from 0.");
    if (operator === "gte" && count < 1) throw new QueryError("\"At least\" needs a count of 1 or more.");
    if (typeof days !== "number" || !Number.isInteger(days) || days < 1 || days > 3650) throw new QueryError("Set the number of days, from 1 to 3650.");
    if (c.action_id !== undefined && c.action_id !== null) {
      if (!isUuid(c.action_id)) throw new QueryError("Pick an action.");
      return { type: "behaviour", action_id: c.action_id, operator, count, days };
    }
    return { type: "behaviour", event: checkKey(c.event, "event"), operator, count, days };
  });
  return { match, criteria };
}

export async function listCohorts(db: Db, projectId: string): Promise<CohortRow[]> {
  return db.asProject(projectId, async (c) => (await c.query<CohortRow>(`select ${COLS} from pa.cohorts where project_id = $1 order by name`, [projectId])).rows);
}

export async function getCohort(db: Db, projectId: string, id: string): Promise<CohortRow | null> {
  if (!isUuid(id)) return null;
  return db.asProject(projectId, async (c) => (await c.query<CohortRow>(`select ${COLS} from pa.cohorts where project_id = $1 and id = $2`, [projectId, id])).rows[0] ?? null);
}

export async function createCohort(db: Db, projectId: string, input: { name: unknown; kind: unknown; definition?: unknown }): Promise<CohortRow> {
  const name = typeof input.name === "string" ? input.name.trim() : "";
  if (name.length < 1 || name.length > 200) throw new QueryError("Give the segment a name of 1 to 200 characters.");
  if (input.kind !== "static" && input.kind !== "behavioural") throw new QueryError("A segment is built from a file or from what people did.");
  const definition = input.kind === "behavioural" ? parseDefinition(input.definition) : {};
  try {
    return await db.asProject(projectId, async (c) =>
      (await c.query<CohortRow>(`insert into pa.cohorts (project_id, name, kind, definition) values ($1, $2, $3, $4) returning ${COLS}`, [projectId, name, input.kind, JSON.stringify(definition)])).rows[0],
    );
  } catch (err) {
    if ((err as { code?: string }).code === "23505") throw new QueryError("A segment with that name already exists.");
    throw err;
  }
}

export async function deleteCohort(db: Db, projectId: string, id: string): Promise<boolean> {
  if (!isUuid(id)) return false;
  return db.asProject(projectId, async (c) => {
    const n = (await c.query("delete from pa.cohorts where project_id = $1 and id = $2", [projectId, id])).rowCount ?? 0;
    if (n) await bumpVersion(c, projectId);
    return n > 0;
  });
}

async function finish(c: Client, projectId: string, id: string, now: Date): Promise<CohortRow> {
  const r = await c.query<CohortRow>(
    `update pa.cohorts set version = version + 1, computed_at = $3, error = null,
            people_count = (select count(*) from pa.cohort_people where project_id = $1 and cohort_id = $2)
      where project_id = $1 and id = $2 returning ${COLS}`,
    [projectId, id, now.toISOString()],
  );
  await bumpVersion(c, projectId);
  return r.rows[0];
}

/** Recomputes a behavioural cohort into pa.cohort_people in one transaction. */
export async function recomputeCohort(db: Db, projectId: string, id: string, now: Date): Promise<CohortRow> {
  const cohort = await getCohort(db, projectId, id);
  if (!cohort) throw new QueryError("That segment no longer exists.");
  if (cohort.kind !== "behavioural") throw new QueryError("This segment comes from a file. Upload a new file to change it.");
  const def = parseDefinition(cohort.definition);
  return db.asProject(projectId, async (c) => {
    await c.query("set local statement_timeout = 120000");
    const actionIds = def.criteria.flatMap((x) => (x.type === "behaviour" && x.action_id ? [x.action_id] : []));
    const ctx: BuildContext = { projectId, now, actions: await loadActions(c, projectId, actionIds) };
    const P = new Params();
    const proj = P.add(projectId);
    const cid = P.add(id);
    const ctes: string[] = [];
    const joins: string[] = [];
    const conds: string[] = [];
    def.criteria.forEach((cr, i) => {
      if (cr.type === "person") {
        conds.push(compareProperty("p.properties", cr.key, cr.operator, cr.value, P));
        return;
      }
      const J = new Joins();
      const s: Series = cr.action_id ? { kind: "action", action_id: cr.action_id } : { kind: "event", event: cr.event };
      const cond = seriesSql(s, ctx, P, J, proj);
      const since = P.add(new Date(now.getTime() - cr.days * 86_400_000).toISOString());
      ctes.push(`b${i} as (
  select ${PERSON} as pid, count(*) as n
    from pa.events e
    ${PERSON_JOIN}
    ${J.sql()}
   where e.project_id = ${proj} and e."timestamp" >= ${since}::timestamptz and e."timestamp" <= ${P.add(now.toISOString())}::timestamptz and ${cond}
   group by 1
)`);
      joins.push(`left join b${i} on b${i}.pid = p.id`);
      conds.push(`coalesce(b${i}.n, 0) ${cr.operator === "gte" ? ">=" : "<="} ${P.add(cr.count)}`);
    });
    await c.query("delete from pa.cohort_people where project_id = $1 and cohort_id = $2", [projectId, id]);
    await c.query(
      `${ctes.length ? `with ${ctes.join(",\n")}` : ""}
insert into pa.cohort_people (project_id, cohort_id, person_id)
select ${proj}::uuid, ${cid}::uuid, p.id
  from pa.persons p
  ${joins.join("\n  ")}
 where p.project_id = ${proj} and (${conds.join(def.match === "all" ? " and " : " or ")})`,
      P.values,
    );
    return finish(c, projectId, id, now);
  });
}

/** A small RFC 4180 reader: quoted fields, doubled quotes, CRLF. */
export function parseCsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') {
        field += '"';
        i += 1;
      } else if (ch === '"') quoted = false;
      else field += ch;
    } else if (ch === '"' && field === "") quoted = true;
    else if (ch === ",") {
      row.push(field);
      field = "";
    } else if (ch === "\n" || ch === "\r") {
      if (ch === "\r" && text[i + 1] === "\n") i += 1;
      row.push(field);
      if (row.some((x) => x !== "")) rows.push(row);
      row = [];
      field = "";
    } else field += ch;
  }
  row.push(field);
  if (row.some((x) => x !== "")) rows.push(row);
  return rows;
}

/** Replaces a static cohort's members with the people named in the CSV. */
export async function importCohortCsv(db: Db, projectId: string, id: string, csv: string, now: Date): Promise<CohortRow & { unmatched: number }> {
  if (Buffer.byteLength(csv, "utf8") > MAX_CSV_BYTES) throw new QueryError("Keep the file under 5 MB.");
  const cohort = await getCohort(db, projectId, id);
  if (!cohort) throw new QueryError("That segment no longer exists.");
  if (cohort.kind !== "static") throw new QueryError("This segment is built from what people did. Create a segment from a file instead.");
  const rows = parseCsv(csv.replace(/^﻿/, ""));
  if (rows.length < 2) throw new QueryError("The file needs a header row and at least one person.");
  const header = rows[0].map((h) => h.trim().toLowerCase());
  const col = header.indexOf("distinct_id") >= 0 ? header.indexOf("distinct_id") : header.indexOf("email");
  if (col < 0) throw new QueryError("Name a column distinct_id or email in the first row.");
  const byEmail = header[col] === "email";
  const values = [...new Set(rows.slice(1).map((r) => (r[col] ?? "").trim()).filter((v) => v.length > 0 && v.length <= 200))];
  if (values.length > MAX_CSV_ROWS) throw new QueryError("Keep the file under 100 000 people.");
  return db.asProject(projectId, async (c) => {
    await c.query("delete from pa.cohort_people where project_id = $1 and cohort_id = $2", [projectId, id]);
    const matched = byEmail
      ? await c.query(
          `with v as (select distinct lower(x) as x from unnest($3::text[]) as x),
                ins as (insert into pa.cohort_people (project_id, cohort_id, person_id)
                        select distinct $1::uuid, $2::uuid, p.id from pa.persons p join v on v.x = lower(p.properties ->> 'email')
                         where p.project_id = $1 on conflict do nothing returning 1)
           select (select count(*) from v where exists (select 1 from pa.persons p where p.project_id = $1 and lower(p.properties ->> 'email') = v.x)) as n`,
          [projectId, id, values],
        )
      : await c.query(
          `with ins as (insert into pa.cohort_people (project_id, cohort_id, person_id)
                        select distinct $1::uuid, $2::uuid, d.person_id from pa.person_distinct_ids d
                         where d.project_id = $1 and d.distinct_id = any($3::text[]) on conflict do nothing returning 1)
           select (select count(*) from pa.person_distinct_ids d where d.project_id = $1 and d.distinct_id = any($3::text[])) as n`,
          [projectId, id, values],
        );
    const found = Number((matched.rows[0] as { n: string }).n);
    const row = await finish(c, projectId, id, now);
    return { ...row, unmatched: values.length - found };
  });
}

/** Recomputes every behavioural cohort of every project (the nightly job). Returns counts per cohort. */
export async function recomputeAll(db: Db, now: Date): Promise<Record<string, number>> {
  const all = await db.query<{ project_id: string; id: string }>("select project_id, id from pa.cohorts where kind = 'behavioural' order by project_id, name");
  const out: Record<string, number> = {};
  for (const x of all) {
    try {
      out[x.id] = (await recomputeCohort(db, x.project_id, x.id, now)).people_count;
    } catch (err) {
      await db.asProject(x.project_id, (c) => c.query("update pa.cohorts set error = $3 where project_id = $1 and id = $2", [x.project_id, x.id, String((err as Error).message).slice(0, 500)]));
    }
  }
  return out;
}

/** Privacy: removes the person from every cohort, drops cached answers and rebuilds the project's rollups. */
export const cohortsGdprHook: GdprHook = {
  name: "cohorts",
  async deletePerson(ref: PersonRef): Promise<number> {
    return ref.db.asProject(ref.projectId, async (c) => {
      const n = (await c.query("delete from pa.cohort_people where project_id = $1 and person_id = any($2::uuid[])", [ref.projectId, ref.personIds])).rowCount ?? 0;
      await c.query("delete from pa.insight_cache where project_id = $1", [ref.projectId]);
      await c.query(
        `insert into pa.rollup_state (project_id, rolled_to, reconcile_after) values ($1, 'epoch', $2)
         on conflict (project_id) do update set reconcile_after = excluded.reconcile_after`,
        [ref.projectId, ref.now.toISOString()],
      );
      await bumpVersion(c, ref.projectId);
      return n;
    });
  },
  async exportPerson(ref: PersonRef): Promise<unknown[]> {
    return ref.db.asProject(ref.projectId, async (c) =>
      (
        await c.query(
          "select k.id as cohort_id, k.name from pa.cohort_people cp join pa.cohorts k on k.id = cp.cohort_id and k.project_id = cp.project_id where cp.project_id = $1 and cp.person_id = any($2::uuid[]) order by k.name",
          [ref.projectId, ref.personIds],
        )
      ).rows,
    );
  },
};
