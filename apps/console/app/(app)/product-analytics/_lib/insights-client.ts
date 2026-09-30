// Server-side client for the insights API of the product analytics service
// (/v1/projects/:id/insights/*), plus the pure mapping between the insight
// builder's form fields and a query. Every call carries a fresh org token
// (A1's paFetch); nothing here runs in the browser.

import { signOrgToken } from "./token.ts";
import { paFetch, PaError, type PaConfig, type Scope } from "./http.ts";

export type Interval = "hour" | "day" | "week" | "month";
export type Kind = "trends" | "funnels" | "retention" | "paths" | "stickiness" | "lifecycle";
export const KINDS: { kind: Kind; label: string }[] = [
  { kind: "trends", label: "Trends" },
  { kind: "funnels", label: "Funnel" },
  { kind: "retention", label: "Retention" },
  { kind: "paths", label: "Paths" },
  { kind: "stickiness", label: "Stickiness" },
  { kind: "lifecycle", label: "Lifecycle" },
];

export type Query = Record<string, unknown> & { kind: Kind };

export interface TrendLine {
  label: string;
  series_index: number;
  breakdown: string | null;
  data: number[];
  total: number | null;
}
export type Result =
  | { kind: "trends"; interval: Interval; labels: string[]; series: TrendLine[]; previous?: { labels: string[]; series: TrendLine[] } }
  | {
      kind: "funnels";
      groups: { breakdown: string | null; breakdown_value: string | null; steps: { step: number; name: string; people: number; from_first: number; from_previous: number; dropped: number; avg_seconds: number | null; median_seconds: number | null }[] }[];
    }
  | { kind: "retention"; period: "day" | "week" | "month"; cohorts: { label: string; size: number; values: number[] }[] }
  | { kind: "paths"; edges: { source_step: number; source: string; target: string; people: number; avg_seconds: number | null; ends: number }[] }
  | { kind: "stickiness"; interval: Interval; series: { label: string; data: number[] }[] }
  | { kind: "lifecycle"; interval: Interval; labels: string[]; statuses: Record<"new" | "returning" | "resurrecting" | "dormant", number[]> };

export interface Answer {
  result: Result;
  computed_at: string;
  cached: boolean;
  stale: boolean;
  took_ms: number;
}

export interface Meta {
  events: { event: string; count: number }[];
  event_properties: string[];
  person_properties: string[];
  group_types: { index: number; name: string }[];
  actions: { id: string; name: string }[];
  segments: { id: string; name: string; people_count: number }[];
}

export interface SavedInsight {
  id: string;
  name: string;
  kind: Kind;
  query: Query;
  created_at: string;
  updated_at: string;
}

export interface DashboardSummary {
  id: string;
  name: string;
  created_at: string;
  tiles: number;
}

export interface Dashboard {
  id: string;
  name: string;
  created_at: string;
  tiles: { position: number; insight: SavedInsight; answer: Answer | null; error: string | null }[];
}

export interface Cohort {
  id: string;
  name: string;
  kind: "static" | "behavioural";
  definition: Record<string, unknown>;
  people_count: number;
  computed_at: string | null;
  error: string | null;
}

export interface Action {
  id: string;
  name: string;
  steps: Record<string, unknown>[];
}

export interface SqlResult {
  columns: string[];
  rows: unknown[][];
  truncated: boolean;
}

export interface FunnelPerson {
  id: string;
  distinct_id: string | null;
  entered_at: string;
  properties: Record<string, unknown> | null;
}

const base = (projectId: string) => `/v1/projects/${projectId}/insights`;
const post = (json: unknown): RequestInit => ({ method: "POST", body: JSON.stringify(json) });

export function insightsApi(cfg: PaConfig, scope: Scope, projectId: string, fetchImpl: typeof fetch = fetch) {
  const call = <T>(path: string, init: RequestInit = {}) => paFetch<T>(cfg, scope, `${base(projectId)}${path}`, init, fetchImpl);
  return {
    meta: () => call<Meta>("/meta"),
    query: (query: Query, refresh = false) => call<Answer>("/query", post({ query, refresh })),
    funnelPeople: (query: Query, step: number, dropped: boolean) => call<{ people: FunnelPerson[] }>("/funnel-people", post({ query, step, dropped })),
    saved: () => call<{ insights: SavedInsight[] }>("/saved"),
    savedOne: (id: string) => call<{ insight: SavedInsight }>(`/saved/${encodeURIComponent(id)}`),
    save: (name: string, query: Query) => call<{ insight: SavedInsight }>("/saved", post({ name, query })),
    deleteSaved: (id: string) => call<{ deleted: boolean }>(`/saved/${encodeURIComponent(id)}`, { method: "DELETE" }),
    dashboards: () => call<{ dashboards: DashboardSummary[] }>("/dashboards"),
    dashboard: (id: string, refresh = false) => call<{ dashboard: Dashboard }>(`/dashboards/${encodeURIComponent(id)}${refresh ? "?refresh=1" : ""}`),
    createDashboard: (name: string) => call<{ dashboard: DashboardSummary }>("/dashboards", post({ name })),
    addTile: (dashboardId: string, insightId: string) => call<{ added: boolean }>(`/dashboards/${encodeURIComponent(dashboardId)}/tiles`, post({ insight_id: insightId })),
    cohorts: () => call<{ segments: Cohort[] }>("/segments"),
    createCohort: (input: { name: string; kind: "static" | "behavioural"; definition?: unknown }) => call<{ segment: Cohort }>("/segments", post(input)),
    uploadCohort: (id: string, csv: string) => call<{ segment: Cohort & { unmatched: number } }>(`/segments/${encodeURIComponent(id)}/csv`, { method: "POST", body: csv, headers: { "content-type": "text/csv" } }),
    recomputeCohort: (id: string) => call<{ segment: Cohort }>(`/segments/${encodeURIComponent(id)}/recompute`, { method: "POST" }),
    actions: () => call<{ actions: Action[] }>("/actions"),
    createAction: (name: string, steps: Record<string, unknown>[]) => call<{ action: Action }>("/actions", post({ name, steps })),
    sql: (sql: string) => call<SqlResult>("/sql", post({ sql })),
    /** The CSV download as a raw response (status and body passed through). */
    async sqlCsv(sql: string, now = new Date()): Promise<Response> {
      const token = await signOrgToken(cfg.secret, { org_id: scope.org_id, app_ids: scope.app_ids, role: scope.role ?? "owner" }, now);
      return fetchImpl(`${cfg.url}${base(projectId)}/sql.csv`, { method: "POST", body: JSON.stringify({ sql }), headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, cache: "no-store" });
    },
  };
}

export const problemText = (e: unknown) => (e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.");

// ---- The builder form <-> query mapping (pure; tested) ----

export const RANGES = [
  { value: "-1d", label: "Since yesterday" },
  { value: "-7d", label: "Last 7 days" },
  { value: "-30d", label: "Last 30 days" },
  { value: "-90d", label: "Last 90 days" },
  { value: "-12m", label: "Last 12 months" },
];

export type Form = Record<string, string | undefined>;

const clean = (v: string | undefined) => (v ?? "").trim();
const int = (v: string | undefined, d: number, lo: number, hi: number) => {
  const n = Number(clean(v));
  return Number.isInteger(n) && n >= lo && n <= hi ? n : d;
};
const oneOf = <T extends string>(v: string | undefined, list: readonly T[], d: T): T => (list.includes(clean(v) as T) ? (clean(v) as T) : d);

function seriesFrom(v: string | undefined, math?: string, prop?: string): Record<string, unknown> | null {
  const s = clean(v);
  if (!s) return null;
  const out: Record<string, unknown> = s.startsWith("action:") ? { kind: "action", action_id: s.slice(7) } : { kind: "event", event: s === "$all" ? null : s };
  if (math && math !== "total") out.math = math;
  if (prop) out.math_property = prop;
  return out;
}

/** The query a builder form describes (fields: kind, range, interval, e1..e3, m1..m3, p1..p3, breakdown, cohort, ...). */
export function formToQuery(f: Form): Query {
  const kind = oneOf(f.kind, ["trends", "funnels", "retention", "paths", "stickiness", "lifecycle"] as const, "trends");
  const properties: Record<string, unknown>[] = [];
  if (clean(f.segment)) properties.push({ type: "segment", segment_id: clean(f.segment) });
  const pk = clean(f.pkey);
  if (pk && clean(f.pval)) properties.push({ type: "person", key: pk, operator: "exact", value: clean(f.pval) });
  const q: Query = { kind, date_range: { date_from: oneOf(f.range, RANGES.map((r) => r.value), "-30d") }, properties };
  const interval = oneOf(f.interval, ["hour", "day", "week", "month"] as const, "day");
  const breakdown = clean(f.breakdown) ? { type: clean(f.breakdown).startsWith("person:") ? "person" : "event", key: clean(f.breakdown).replace(/^(person|event):/, "") } : null;
  const events = [1, 2, 3, 4, 5].map((i) => seriesFrom(f[`e${i}`], clean(f[`m${i}`]) || undefined, clean(f[`p${i}`]) || undefined)).filter((s): s is Record<string, unknown> => s !== null);
  switch (kind) {
    case "trends":
      return { ...q, interval, series: events.length ? events.slice(0, 3) : [{ kind: "event", event: "$pageview" }], breakdown, formula: clean(f.formula) || null, compare: f.compare === "on" };
    case "funnels":
      return {
        ...q,
        steps: events.length >= 2 ? events.map(({ math: _m, math_property: _p, ...s }) => s) : [{ kind: "event", event: "$pageview" }, { kind: "event", event: "signup" }],
        order: oneOf(f.order, ["ordered", "strict", "unordered"] as const, "ordered"),
        window: { value: int(f.window, 14, 1, 10_000), unit: oneOf(f.wunit, ["minute", "hour", "day", "week"] as const, "day") },
        breakdown,
      };
    case "retention": {
      const [start, ret] = events;
      return {
        ...q,
        start: start ?? { kind: "event", event: "$pageview" },
        return: ret ?? start ?? { kind: "event", event: "$pageview" },
        period: oneOf(f.period, ["day", "week", "month"] as const, "week"),
        intervals: int(f.intervals, 8, 1, 31),
        retention_type: oneOf(f.rtype, ["first_time", "recurring"] as const, "first_time"),
      };
    }
    case "paths":
      return { ...q, path_type: oneOf(f.ptype, ["pageview", "screen", "custom", "all"] as const, "pageview"), start_point: clean(f.start) || null, end_point: clean(f.end) || null, depth: int(f.depth, 5, 2, 10) };
    case "stickiness":
      return { ...q, interval, series: events.length ? events.slice(0, 3) : [{ kind: "event", event: "$pageview" }] };
    case "lifecycle":
      return { ...q, interval, series: [events[0] ?? { kind: "event", event: "$pageview" }] };
  }
}

/** The builder fields that reproduce a saved query (the inverse of formToQuery for what the form can express). */
export function queryToForm(q: Query): Form {
  const f: Form = { kind: q.kind };
  const r = q.date_range as { date_from?: string } | undefined;
  if (r?.date_from) f.range = r.date_from;
  if (typeof q.interval === "string") f.interval = q.interval;
  const props = (q.properties as Record<string, unknown>[] | undefined) ?? [];
  for (const p of props) {
    if (p.type === "segment") f.segment = String(p.segment_id);
    if (p.type === "person") {
      f.pkey = String(p.key);
      f.pval = String(p.value);
    }
  }
  const list = (q.series ?? q.steps ?? (q.kind === "retention" ? [q.start, q.return] : [])) as Record<string, unknown>[];
  list.forEach((s, i) => {
    if (!s) return;
    f[`e${i + 1}`] = s.kind === "action" ? `action:${s.action_id}` : s.event === null ? "$all" : String(s.event);
    if (s.math) f[`m${i + 1}`] = String(s.math);
    if (s.math_property) f[`p${i + 1}`] = String(s.math_property);
  });
  const b = q.breakdown as { type: string; key: string } | null | undefined;
  if (b) f.breakdown = `${b.type}:${b.key}`;
  if (typeof q.formula === "string") f.formula = q.formula;
  if (q.compare === true) f.compare = "on";
  if (typeof q.order === "string") f.order = q.order;
  const w = q.window as { value: number; unit: string } | undefined;
  if (w) {
    f.window = String(w.value);
    f.wunit = w.unit;
  }
  if (typeof q.period === "string") f.period = q.period;
  if (typeof q.intervals === "number") f.intervals = String(q.intervals);
  if (typeof q.retention_type === "string") f.rtype = q.retention_type;
  if (typeof q.path_type === "string") f.ptype = q.path_type;
  if (typeof q.start_point === "string") f.start = q.start_point;
  if (typeof q.end_point === "string") f.end = q.end_point;
  if (typeof q.depth === "number") f.depth = String(q.depth);
  return f;
}

/** The builder's URL for a query (links from saved insights and dashboards). */
export function builderHref(q: Query, app: string): string {
  const params = new URLSearchParams({ app });
  for (const [k, v] of Object.entries(queryToForm(q))) if (v) params.set(k, v);
  return `/product-analytics/insights/new?${params.toString()}`;
}

export function fmtAt(iso: string): string {
  return new Date(iso).toISOString().replace("T", " ").slice(0, 16) + " UTC";
}
