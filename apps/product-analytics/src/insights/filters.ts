// Property filters, series matching and action steps as SQL conditions over
// `pa.events e`. Keys and values are bound; the joins a condition needs
// (the person row, group rows) are recorded on `Joins` so the builder adds
// them once.

import { HREF_PATTERN, selectorRegex, tagNameRegex, TEXT_PATTERN } from "./elements.ts";
import { PERSON, type Params } from "./sql.ts";
import { QueryError, type ActionStep, type BuildContext, type PropertyFilter, type PropertyOperator, type Series } from "./types.ts";

export class Joins {
  person = false;
  groups = new Set<number>();

  /** Join clauses for everything the conditions used. `e` must be the events alias. */
  sql(): string {
    const out: string[] = [];
    if (this.person) out.push(`left join pa.persons p on p.project_id = e.project_id and p.id = ${PERSON}`);
    for (const g of [...this.groups].sort()) {
      out.push(`left join pa.groups g${g} on g${g}.project_id = e.project_id and g${g}.group_type_index = ${g} and g${g}.group_key = e."$group_${g}"`);
    }
    return out.join("\n");
  }
}

const OPERATORS: PropertyOperator[] = ["exact", "is_not", "icontains", "not_icontains", "regex", "not_regex", "gt", "gte", "lt", "lte", "is_set", "is_not_set"];
const NUMERIC = "'^\\s*-?[0-9]+(\\.[0-9]+)?([eE][-+]?[0-9]+)?\\s*$'";

/** A text value as a number, or null when it is not numeric. */
export const asNumber = (text: string) => `(case when (${text}) ~ ${NUMERIC} then (${text})::double precision end)`;

const likeEscape = (s: string) => s.replace(/[\\%_]/g, (m) => `\\${m}`);

function textValues(v: unknown): string[] {
  const arr = Array.isArray(v) ? v : [v];
  if (arr.length === 0 || arr.length > 100) throw new QueryError("Give between 1 and 100 values to match.");
  return arr.map((x) => {
    if (x === null || x === undefined) throw new QueryError("Give a value to match.");
    return typeof x === "string" ? x : String(x);
  });
}

function oneValue(v: unknown): string {
  const [x] = textValues(Array.isArray(v) ? v.slice(0, 1) : v);
  return x;
}

function numberValue(v: unknown): number {
  const n = typeof v === "number" ? v : Number(Array.isArray(v) ? v[0] : v);
  if (!Number.isFinite(n)) throw new QueryError("Compare with a number.");
  return n;
}

/** One comparison over a jsonb column and a bound key. */
export function compareProperty(json: string, key: string, op: PropertyOperator, value: unknown, P: Params): string {
  if (!OPERATORS.includes(op)) throw new QueryError("Pick a filter operator from the list.");
  const k = P.add(key);
  const text = `(${json} ->> ${k})`;
  switch (op) {
    case "exact":
      return `${text} = any(${P.add(textValues(value))}::text[])`;
    case "is_not":
      return `(${text} is null or not (${text} = any(${P.add(textValues(value))}::text[])))`;
    case "icontains":
      return `${text} ilike ${P.add(`%${likeEscape(oneValue(value))}%`)}`;
    case "not_icontains":
      return `(${text} is null or ${text} not ilike ${P.add(`%${likeEscape(oneValue(value))}%`)})`;
    case "regex":
      return `${text} ~ ${P.add(oneValue(value))}`;
    case "not_regex":
      return `(${text} is null or ${text} !~ ${P.add(oneValue(value))})`;
    case "gt":
      return `${asNumber(text)} > ${P.add(numberValue(value))}::double precision`;
    case "gte":
      return `${asNumber(text)} >= ${P.add(numberValue(value))}::double precision`;
    case "lt":
      return `${asNumber(text)} < ${P.add(numberValue(value))}::double precision`;
    case "lte":
      return `${asNumber(text)} <= ${P.add(numberValue(value))}::double precision`;
    case "is_set":
      return `(${json} ? ${k})`;
    case "is_not_set":
      return `not coalesce(${json} ? ${k}, false)`;
  }
}

/** Members of a cohort as resolved person ids (a merged member still matches). */
export function cohortMembers(cohortId: string, projectParam: string, P: Params): string {
  return `(select coalesce(co.new_person_id, cp.person_id) from pa.cohort_people cp
            left join pa.person_overrides co on co.project_id = cp.project_id and co.old_person_id = cp.person_id
           where cp.project_id = ${projectParam} and cp.cohort_id = ${P.add(cohortId)}::uuid)`;
}

export function filterSql(f: PropertyFilter, P: Params, J: Joins, projectParam: string): string {
  switch (f.type) {
    case "event":
      return compareProperty("e.properties", f.key, f.operator, f.value, P);
    case "person":
      J.person = true;
      return compareProperty("p.properties", f.key, f.operator, f.value, P);
    case "group":
      if (!Number.isInteger(f.group_type_index) || f.group_type_index < 0 || f.group_type_index > 4) throw new QueryError("Pick a group type.");
      J.groups.add(f.group_type_index);
      return compareProperty(`g${f.group_type_index}.properties`, f.key, f.operator, f.value, P);
    case "cohort":
      return `${f.negate ? "not " : ""}(${PERSON} in ${cohortMembers(f.cohort_id, projectParam, P)})`;
  }
}

export function filtersSql(fs: PropertyFilter[] | undefined, P: Params, J: Joins, projectParam: string): string {
  if (!fs?.length) return "true";
  return fs.map((f) => filterSql(f, P, J, projectParam)).join(" and ");
}

function matching(col: string, value: string, how: "exact" | "contains" | "regex" | undefined, P: Params): string {
  if (how === "regex") return `${col} ~ ${P.add(value)}`;
  if (how === "contains") return `${col} ilike ${P.add(`%${likeEscape(value)}%`)}`;
  return `${col} = ${P.add(value)}`;
}

/** One action step (upstream steps_to_expr): every part of the step must hold. */
export function stepSql(step: ActionStep, P: Params, J: Joins, projectParam: string): string {
  const parts: string[] = [];
  if (step.event) parts.push(`e.event = ${P.add(step.event)}`);
  if (step.event === "$autocapture") {
    if (step.selector) {
      const re = selectorRegex(step.selector);
      if (re) parts.push(`coalesce(e.elements_chain, '') ~ ${P.add(re)}`);
    }
    if (step.tag_name) parts.push(`coalesce(e.elements_chain, '') ~ ${P.add(tagNameRegex(step.tag_name))}`);
    if (step.href != null) parts.push(matching(`coalesce(substring(e.elements_chain from ${P.add(HREF_PATTERN)}), '')`, step.href, step.href_matching, P));
    if (step.text != null) {
      const cmp = matching("m.v[1]", step.text, step.text_matching, P);
      parts.push(`exists (select 1 from regexp_matches(coalesce(e.elements_chain, ''), ${P.add(TEXT_PATTERN)}, 'g') as m(v) where ${cmp})`);
    }
  }
  if (step.url) {
    const how = step.url_matching ?? "contains";
    parts.push(matching(`(e.properties ->> '$current_url')`, step.url, how === "contains" ? "contains" : how, P));
  }
  if (step.properties?.length) parts.push(filtersSql(step.properties, P, J, projectParam));
  return parts.length ? `(${parts.join(" and ")})` : "true";
}

/** Events a series counts: its event (or action), and its own filters. */
export function seriesSql(s: Series, ctx: BuildContext, P: Params, J: Joins, projectParam: string): string {
  const parts: string[] = [];
  if (s.kind === "action") {
    const action = s.action_id ? ctx.actions.get(s.action_id) : undefined;
    if (!action) throw new QueryError("That action no longer exists. Pick another one.");
    if (!action.steps.length) parts.push("false");
    else parts.push(`(${action.steps.map((st) => stepSql(st, P, J, projectParam)).join(" or ")})`);
  } else if (s.event) {
    parts.push(`e.event = ${P.add(s.event)}`);
  }
  if (s.properties?.length) parts.push(filtersSql(s.properties, P, J, projectParam));
  return parts.length ? parts.join(" and ") : "true";
}

/** A bound, safe expression for a breakdown value. */
export function breakdownValue(b: { type: "event" | "person" | "group"; key: string; group_type_index?: number }, P: Params, J: Joins): string {
  const k = P.add(b.key);
  if (b.type === "person") {
    J.person = true;
    return `(p.properties ->> ${k})`;
  }
  if (b.type === "group") {
    const g = b.group_type_index ?? 0;
    if (!Number.isInteger(g) || g < 0 || g > 4) throw new QueryError("Pick a group type.");
    J.groups.add(g);
    return `(g${g}.properties ->> ${k})`;
  }
  return `(e.properties ->> ${k})`;
}

/** Label for a missing breakdown value, and for the values past the limit. */
export const NONE_LABEL = "(none)";
export const OTHER_LABEL = "(other)";
