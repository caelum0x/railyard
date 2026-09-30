// Untrusted JSON -> a typed insight query, or a QueryError with a plain
// message. Only known fields are copied; everything else is dropped.

import { isUuid } from "../db.ts";
import { checkKey, checkTimezone, INTERVALS } from "./sql.ts";
import {
  INSIGHT_KINDS,
  QueryError,
  type ActionStep,
  type Breakdown,
  type DateRange,
  type InsightQuery,
  type MathKind,
  type PropertyFilter,
  type PropertyOperator,
  type PropertyValue,
  type Series,
} from "./types.ts";

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

const MATH: MathKind[] = ["total", "dau", "weekly_active", "monthly_active", "unique_group", "sum", "avg", "min", "max", "median", "p75", "p90", "p95", "p99"];
const OPS: PropertyOperator[] = ["exact", "is_not", "icontains", "not_icontains", "regex", "not_regex", "gt", "gte", "lt", "lte", "is_set", "is_not_set"];
const MAX_FILTERS = 20;
const MAX_SERIES = 10;

function oneOf<T extends string>(v: unknown, allowed: readonly T[], fallback: T | undefined, what: string): T {
  if (v === undefined || v === null) {
    if (fallback !== undefined) return fallback;
    throw new QueryError(`Pick ${what}.`);
  }
  if (typeof v !== "string" || !allowed.includes(v as T)) throw new QueryError(`Pick ${what} from: ${allowed.join(", ")}.`);
  return v as T;
}

function groupIndex(v: unknown): number {
  if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > 4) throw new QueryError("Pick a group type.");
  return v;
}

function filterValue(v: unknown): PropertyValue {
  if (v === undefined || v === null) return null;
  if (typeof v === "string") return v.length > 1000 ? v.slice(0, 1000) : v;
  if (typeof v === "number" || typeof v === "boolean") return v;
  if (Array.isArray(v) && v.length <= 100 && v.every((x) => typeof x === "string" || typeof x === "number")) return v as (string | number)[];
  throw new QueryError("A filter value must be text, a number, or a list of them.");
}

export function parseFilters(v: unknown): PropertyFilter[] {
  if (v === undefined || v === null) return [];
  if (!Array.isArray(v) || v.length > MAX_FILTERS) throw new QueryError(`Use at most ${MAX_FILTERS} filters.`);
  return v.map((f): PropertyFilter => {
    if (!isObj(f)) throw new QueryError("A filter must be an object.");
    const type = oneOf(f.type ?? "event", ["event", "person", "group", "cohort"] as const, "event", "a filter type");
    if (type === "cohort") {
      if (!isUuid(f.cohort_id)) throw new QueryError("Pick a cohort.");
      return { type, cohort_id: f.cohort_id, negate: f.negate === true };
    }
    const operator = oneOf(f.operator, OPS, "exact", "an operator");
    const key = checkKey(f.key);
    const value = filterValue(f.value);
    if (operator !== "is_set" && operator !== "is_not_set" && (value === null || (Array.isArray(value) && value.length === 0))) throw new QueryError(`Give a value for the ${key} filter.`);
    if (type === "group") return { type, key, operator, value, group_type_index: groupIndex(f.group_type_index) };
    return { type, key, operator, value };
  });
}

export function parseSeries(v: unknown, what = "an event"): Series {
  if (!isObj(v)) throw new QueryError(`Pick ${what}.`);
  const kind = oneOf(v.kind ?? (v.action_id ? "action" : "event"), ["event", "action"] as const, "event", "an event or action");
  const s: Series = { kind };
  if (kind === "action") {
    if (!isUuid(v.action_id)) throw new QueryError("Pick an action.");
    s.action_id = v.action_id;
  } else {
    s.event = v.event === null || v.event === undefined ? null : checkKey(v.event, "event");
  }
  if (typeof v.name === "string") s.name = v.name.slice(0, 200);
  s.math = oneOf(v.math, MATH, "total", "a measure");
  if (v.math_property !== undefined && v.math_property !== null) s.math_property = checkKey(v.math_property);
  if (v.math_group_type_index !== undefined && v.math_group_type_index !== null) s.math_group_type_index = groupIndex(v.math_group_type_index);
  s.properties = parseFilters(v.properties);
  return s;
}

function parseSeriesList(v: unknown, min: number, max: number, what: string): Series[] {
  if (!Array.isArray(v) || v.length < min || v.length > max) throw new QueryError(`Add ${min === max ? min : `${min} to ${max}`} ${what}.`);
  return v.map((s) => parseSeries(s));
}

function parseRange(v: unknown): DateRange {
  if (!isObj(v)) return { date_from: "-30d" };
  const from = v.date_from ?? "-30d";
  if (typeof from !== "string" || from.length > 40) throw new QueryError("Pick a start date.");
  const to = v.date_to;
  if (to !== undefined && to !== null && (typeof to !== "string" || to.length > 40)) throw new QueryError("Pick an end date.");
  return { date_from: from, date_to: (to as string | null | undefined) ?? null };
}

function parseBreakdown(v: unknown): Breakdown | null {
  if (v === undefined || v === null) return null;
  if (!isObj(v)) throw new QueryError("A breakdown needs a property.");
  const type = oneOf(v.type ?? "event", ["event", "person", "group"] as const, "event", "a breakdown type");
  const b: Breakdown = { type, key: checkKey(v.key) };
  if (type === "group") b.group_type_index = groupIndex(v.group_type_index);
  if (v.limit !== undefined) {
    if (typeof v.limit !== "number" || !Number.isInteger(v.limit)) throw new QueryError("The breakdown limit is a whole number.");
    b.limit = v.limit;
  }
  return b;
}

function intIn(v: unknown, lo: number, hi: number, fallback: number, what: string): number {
  if (v === undefined || v === null) return fallback;
  if (typeof v !== "number" || !Number.isInteger(v) || v < lo || v > hi) throw new QueryError(`${what} must be a whole number from ${lo} to ${hi}.`);
  return v;
}

export function parseQuery(v: unknown): InsightQuery {
  if (!isObj(v)) throw new QueryError("Send an insight query.");
  const kind = oneOf(v.kind, INSIGHT_KINDS, undefined, "an insight type");
  const base = { date_range: parseRange(v.date_range), properties: parseFilters(v.properties), timezone: checkTimezone(typeof v.timezone === "string" ? v.timezone : undefined) };
  const interval = oneOf(v.interval, INTERVALS, "day", "an interval");
  switch (kind) {
    case "trends": {
      const formula = typeof v.formula === "string" && v.formula.trim() ? v.formula.trim() : null;
      return { kind, ...base, interval, series: parseSeriesList(v.series, 1, MAX_SERIES, "series"), breakdown: parseBreakdown(v.breakdown), formula, compare: v.compare === true };
    }
    case "funnels": {
      const w = isObj(v.window) ? v.window : {};
      return {
        kind,
        ...base,
        steps: parseSeriesList(v.steps, 2, 10, "steps"),
        order: oneOf(v.order, ["ordered", "strict", "unordered"] as const, "ordered", "a step order"),
        window: { value: intIn(w.value, 1, 10_000, 14, "The conversion window"), unit: oneOf(w.unit, ["minute", "hour", "day", "week"] as const, "day", "a window unit") },
        breakdown: parseBreakdown(v.breakdown),
        aggregation_group_type_index: v.aggregation_group_type_index === undefined || v.aggregation_group_type_index === null ? null : groupIndex(v.aggregation_group_type_index),
      };
    }
    case "retention":
      return {
        kind,
        ...base,
        start: parseSeries(v.start, "the start event"),
        return: parseSeries(v.return ?? v.start, "the return event"),
        period: oneOf(v.period, ["day", "week", "month"] as const, "day", "a period"),
        intervals: intIn(v.intervals, 1, 31, 8, "Periods"),
        retention_type: oneOf(v.retention_type, ["first_time", "recurring"] as const, "first_time", "first time or recurring"),
      };
    case "paths": {
      const pt = (x: unknown, what: string) => (x === undefined || x === null || x === "" ? null : checkKey(x, what));
      return {
        kind,
        ...base,
        path_type: oneOf(v.path_type, ["pageview", "screen", "custom", "all"] as const, "pageview", "what a step is"),
        start_point: pt(v.start_point, "start point"),
        end_point: pt(v.end_point, "end point"),
        depth: intIn(v.depth, 2, 10, 5, "Steps"),
        edge_limit: intIn(v.edge_limit, 1, 500, 50, "Links shown"),
      };
    }
    case "stickiness":
      return { kind, ...base, interval, series: parseSeriesList(v.series, 1, MAX_SERIES, "series") };
    case "lifecycle": {
      const [s] = parseSeriesList(v.series, 1, 1, "event");
      return { kind, ...base, interval, series: [s] };
    }
  }
}

const MATCH = ["exact", "contains", "regex"] as const;

export function parseActionSteps(v: unknown): ActionStep[] {
  if (!Array.isArray(v) || v.length < 1 || v.length > 20) throw new QueryError("An action has 1 to 20 matches.");
  return v.map((s): ActionStep => {
    if (!isObj(s)) throw new QueryError("Each match must be an object.");
    const str = (x: unknown, what: string) => (x === undefined || x === null || x === "" ? null : checkKey(x, what));
    const long = (x: unknown, what: string) => {
      if (x === undefined || x === null || x === "") return null;
      if (typeof x !== "string" || x.length > 1000) throw new QueryError(`Keep the ${what} under 1000 characters.`);
      return x;
    };
    const step: ActionStep = {
      event: str(s.event, "event"),
      url: long(s.url, "address"),
      url_matching: oneOf(s.url_matching, MATCH, "contains", "how the address matches"),
      selector: long(s.selector, "selector"),
      tag_name: str(s.tag_name, "tag"),
      text: long(s.text, "text"),
      text_matching: oneOf(s.text_matching, MATCH, "exact", "how the text matches"),
      href: long(s.href, "link"),
      href_matching: oneOf(s.href_matching, MATCH, "exact", "how the link matches"),
      properties: parseFilters(s.properties),
    };
    if (!step.event && !step.url && !step.properties?.length) throw new QueryError("Each match needs an event, an address or a filter.");
    return step;
  });
}
