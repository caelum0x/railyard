// Insight query shapes. The console sends these as JSON; `parseQuery`
// (validate.ts) turns untrusted input into one of them or a plain message.
// Semantics follow the upstream product-analytics query runners (MIT, see
// src/insights/NOTICE): trends, funnels, retention, paths, stickiness and
// lifecycle.

export type Interval = "hour" | "day" | "week" | "month";

export interface DateRange {
  /** ISO date or time, or a relative "-30d" / "-8w" / "-3m" / "-24h". */
  date_from: string;
  /** ISO date or time; default now. A date without a time means the end of that day. */
  date_to?: string | null;
}

export type PropertyOperator =
  | "exact"
  | "is_not"
  | "icontains"
  | "not_icontains"
  | "regex"
  | "not_regex"
  | "gt"
  | "gte"
  | "lt"
  | "lte"
  | "is_set"
  | "is_not_set";

export type PropertyValue = string | number | boolean | (string | number)[] | null;

export type PropertyFilter =
  | {
      type: "event" | "person";
      key: string;
      operator: PropertyOperator;
      value?: PropertyValue;
    }
  | {
      type: "group";
      key: string;
      group_type_index: number;
      operator: PropertyOperator;
      value?: PropertyValue;
    }
  | { type: "cohort"; cohort_id: string; negate?: boolean };

export type MathKind =
  | "total"
  | "dau"
  | "weekly_active"
  | "monthly_active"
  | "unique_group"
  | "sum"
  | "avg"
  | "min"
  | "max"
  | "median"
  | "p75"
  | "p90"
  | "p95"
  | "p99";

export const PROPERTY_MATH: MathKind[] = ["sum", "avg", "min", "max", "median", "p75", "p90", "p95", "p99"];

export interface Series {
  kind: "event" | "action";
  /** Event name; null means every event. Only for kind "event". */
  event?: string | null;
  /** Only for kind "action". */
  action_id?: string;
  name?: string;
  math?: MathKind;
  math_property?: string;
  math_group_type_index?: number;
  properties?: PropertyFilter[];
}

export interface Breakdown {
  type: "event" | "person" | "group";
  key: string;
  group_type_index?: number;
  /** Top values shown; the rest are summed into "(other)". Default 10, at most 25. */
  limit?: number;
}

interface Base {
  date_range: DateRange;
  /** Filters every event of the insight. */
  properties?: PropertyFilter[];
  /** IANA time zone for buckets. Default UTC. */
  timezone?: string;
}

export interface TrendsQuery extends Base {
  kind: "trends";
  series: Series[];
  interval?: Interval;
  breakdown?: Breakdown | null;
  /** Over series letters A, B, ... e.g. "A / B * 100". */
  formula?: string | null;
  compare?: boolean;
}

export type FunnelOrder = "ordered" | "strict" | "unordered";
export type WindowUnit = "minute" | "hour" | "day" | "week";

export interface FunnelsQuery extends Base {
  kind: "funnels";
  steps: Series[];
  order?: FunnelOrder;
  window?: { value: number; unit: WindowUnit };
  breakdown?: Breakdown | null;
  /** Count groups of this type instead of people. */
  aggregation_group_type_index?: number | null;
}

export interface RetentionQuery extends Base {
  kind: "retention";
  start: Series;
  return: Series;
  period?: "day" | "week" | "month";
  /** Number of cohorts (rows) and of periods after the start (columns). */
  intervals?: number;
  retention_type?: "first_time" | "recurring";
}

export interface PathsQuery extends Base {
  kind: "paths";
  path_type?: "pageview" | "screen" | "custom" | "all";
  start_point?: string | null;
  end_point?: string | null;
  depth?: number;
  edge_limit?: number;
}

export interface StickinessQuery extends Base {
  kind: "stickiness";
  series: Series[];
  interval?: Interval;
}

export interface LifecycleQuery extends Base {
  kind: "lifecycle";
  series: [Series];
  interval?: Interval;
}

export type InsightQuery = TrendsQuery | FunnelsQuery | RetentionQuery | PathsQuery | StickinessQuery | LifecycleQuery;
export type InsightKind = InsightQuery["kind"];
export const INSIGHT_KINDS: InsightKind[] = ["trends", "funnels", "retention", "paths", "stickiness", "lifecycle"];

/** A built statement: bound parameters only, never interpolated input. */
export interface Built {
  sql: string;
  params: unknown[];
}

/** An action step (upstream action semantics: OR across steps, AND inside one). */
export interface ActionStep {
  event?: string | null;
  url?: string | null;
  url_matching?: "exact" | "contains" | "regex";
  selector?: string | null;
  tag_name?: string | null;
  text?: string | null;
  text_matching?: "exact" | "contains" | "regex";
  href?: string | null;
  href_matching?: "exact" | "contains" | "regex";
  properties?: PropertyFilter[];
}

export interface ActionDef {
  id: string;
  name: string;
  steps: ActionStep[];
}

/** What builders need to resolve references (actions) without a database round trip. */
export interface BuildContext {
  projectId: string;
  now: Date;
  actions: Map<string, ActionDef>;
}

export class QueryError extends Error {
  status = 400;
}
