// Runs a validated insight query for one project and shapes the answer.
// Every statement runs as the request role with the project set (row level
// security), binds the project id, and has a statement timeout.

import type { Client, Db } from "../db.ts";
import { evalFormula, parseFormula } from "./formula.ts";
import { buildFunnel, buildFunnelPeople, buildTimeToConvert } from "./funnels.ts";
import { NONE_LABEL, OTHER_LABEL } from "./filters.ts";
import { buildLifecycle, buildStickiness } from "./lifecycle.ts";
import { buildPaths } from "./paths.ts";
import { buildRetention } from "./retention.ts";
import { previousRange, resolveRange, type ResolvedRange } from "./sql.ts";
import { rolledTo } from "./rollups.ts";
import { buildAxis, buildBreakdownTop, buildTrendRollup, buildTrendSeries, peopleRollupEligible, rollupEligible } from "./trends.ts";
import type {
  ActionDef,
  BuildContext,
  Built,
  FunnelsQuery,
  InsightQuery,
  Interval,
  LifecycleQuery,
  PathsQuery,
  RetentionQuery,
  Series,
  StickinessQuery,
  TrendsQuery,
} from "./types.ts";
import { QueryError } from "./types.ts";

export const QUERY_TIMEOUT_MS = 30_000;
/** Per insight transaction; the pool runs at most 5, so at most 5 x this at once. */
export const INSIGHT_WORK_MEM = "32MB";

export interface TrendLine {
  label: string;
  series_index: number;
  breakdown: string | null;
  data: number[];
  total: number | null;
}
export interface TrendsResult {
  kind: "trends";
  interval: Interval;
  labels: string[];
  series: TrendLine[];
  previous?: { labels: string[]; series: TrendLine[] };
}
export interface FunnelStepResult {
  step: number;
  name: string;
  people: number;
  from_first: number;
  from_previous: number;
  dropped: number;
  avg_seconds: number | null;
  median_seconds: number | null;
}
export interface FunnelsResult {
  kind: "funnels";
  groups: { breakdown: string | null; breakdown_value: string | null; steps: FunnelStepResult[] }[];
}
export interface RetentionResult {
  kind: "retention";
  period: "day" | "week" | "month";
  cohorts: { label: string; size: number; values: number[] }[];
}
export interface PathsResult {
  kind: "paths";
  edges: { source_step: number; source: string; target: string; people: number; avg_seconds: number | null; ends: number }[];
}
export interface StickinessResult {
  kind: "stickiness";
  interval: Interval;
  series: { label: string; data: number[] }[];
}
export interface LifecycleResult {
  kind: "lifecycle";
  interval: Interval;
  labels: string[];
  statuses: Record<"new" | "returning" | "resurrecting" | "dormant", number[]>;
}
export type InsightResult = TrendsResult | FunnelsResult | RetentionResult | PathsResult | StickinessResult | LifecycleResult;

type Row = Record<string, unknown>;

async function run(c: Client, b: Built): Promise<Row[]> {
  return (await c.query(b.sql, b.params)).rows as Row[];
}

const num = (v: unknown): number => (v === null || v === undefined ? 0 : Number(v));
const numOrNull = (v: unknown): number | null => (v === null || v === undefined ? null : Number(v));

function cut(label: string, interval: Interval): string {
  return interval === "hour" ? label : label.slice(0, 10);
}

export function seriesLabel(s: Series, actions: Map<string, ActionDef>): string {
  if (s.name) return s.name;
  const base = s.kind === "action" ? (actions.get(s.action_id ?? "")?.name ?? "Action") : (s.event ?? "All events");
  const math = s.math ?? "total";
  return math === "total" ? base : `${base} (${math}${s.math_property ? ` of ${s.math_property}` : ""})`;
}

function breakdownLabel(bk: unknown, bv: unknown): string | null {
  const k = Number(bk);
  if (k === 0) return NONE_LABEL;
  if (k === 2) return OTHER_LABEL;
  return bv === null || bv === undefined ? null : String(bv);
}

/** Every action a query refers to. */
export function actionIds(q: InsightQuery): string[] {
  const list: Series[] = [];
  if ("series" in q) list.push(...q.series);
  if (q.kind === "funnels") list.push(...q.steps);
  if (q.kind === "retention") list.push(q.start, q.return);
  return [...new Set(list.filter((s) => s.kind === "action" && s.action_id).map((s) => s.action_id as string))];
}

export async function loadActions(c: Client, projectId: string, ids: string[]): Promise<Map<string, ActionDef>> {
  if (!ids.length) return new Map();
  const r = await c.query<ActionDef>("select id, name, steps from pa.actions where project_id = $1 and id = any($2::uuid[])", [projectId, ids]);
  return new Map(r.rows.map((a) => [a.id, a]));
}

async function trendLines(c: Client, q: TrendsQuery, range: ResolvedRange, ctx: BuildContext): Promise<{ labels: string[]; lines: TrendLine[] }> {
  const interval = q.interval ?? "day";
  const labels = (await run(c, buildAxis(q, range))).map((r) => cut(String(r.bucket), interval));
  const index = new Map(labels.map((l, i) => [l, i]));
  const lines: TrendLine[] = [];
  const watermark = q.series.some((s) => rollupEligible({ ...q, interval }, s, range) || peopleRollupEligible({ ...q, interval }, s)) ? await rolledTo(c, ctx.projectId) : null;
  for (let si = 0; si < q.series.length; si += 1) {
    const s = q.series[si];
    const top = q.breakdown ? (await run(c, buildBreakdownTop(q, s, range, ctx))).map((r) => String(r.bv)) : [];
    const built = watermark && rollupEligible({ ...q, interval }, s, range) ? buildTrendRollup({ ...q, interval }, s, range, ctx, watermark) : buildTrendSeries(q, s, range, ctx, top, watermark);
    const rows = await run(c, built);
    const byKey = new Map<string, TrendLine>();
    for (const r of rows) {
      const b = q.breakdown ? breakdownLabel(r.bk, r.bv) : null;
      const key = `${r.bk}|${r.bv ?? ""}`;
      let line = byKey.get(key);
      if (!line) {
        line = { label: seriesLabel(s, ctx.actions), series_index: si, breakdown: b, data: labels.map(() => 0), total: numOrNull(r.total) };
        byKey.set(key, line);
      }
      const i = index.get(cut(String(r.bucket), interval));
      if (i !== undefined) line.data[i] = num(r.value);
    }
    if (byKey.size === 0) lines.push({ label: seriesLabel(s, ctx.actions), series_index: si, breakdown: null, data: labels.map(() => 0), total: 0 });
    else lines.push(...byKey.values());
  }
  if (!q.formula) return { labels, lines };
  const f = parseFormula(q.formula, q.series.length);
  const keys = [...new Set(lines.map((l) => l.breakdown))];
  const out = keys.map((b): TrendLine => {
    const per = q.series.map((_, si) => lines.find((l) => l.series_index === si && l.breakdown === b));
    return {
      label: q.formula as string,
      series_index: 0,
      breakdown: b,
      data: labels.map((_, i) => evalFormula(f, per.map((l) => l?.data[i] ?? 0))),
      total: evalFormula(f, per.map((l) => l?.total ?? 0)),
    };
  });
  return { labels, lines: out };
}

async function runTrends(c: Client, q: TrendsQuery, range: ResolvedRange, ctx: BuildContext): Promise<TrendsResult> {
  const cur = await trendLines(c, q, range, ctx);
  const out: TrendsResult = { kind: "trends", interval: q.interval ?? "day", labels: cur.labels, series: cur.lines };
  if (q.compare) {
    const prev = await trendLines(c, q, previousRange(range), ctx);
    out.previous = { labels: prev.labels, series: prev.lines };
  }
  return out;
}

async function runFunnels(c: Client, q: FunnelsQuery, range: ResolvedRange, ctx: BuildContext): Promise<FunnelsResult> {
  const rows = await run(c, buildFunnel(q, range, ctx));
  const groups = new Map<string, FunnelsResult["groups"][number]>();
  for (const r of rows) {
    const key = `${r.bk}|${r.bv ?? ""}`;
    let g = groups.get(key);
    if (!g) {
      g = { breakdown: q.breakdown ? breakdownLabel(r.bk, r.bv) : null, breakdown_value: Number(r.bk) === 1 ? String(r.bv) : null, steps: [] };
      groups.set(key, g);
    }
    const step = Number(r.step);
    const s = q.steps[step - 1];
    g.steps.push({
      step,
      name: seriesLabel(s, ctx.actions),
      people: num(r.people),
      from_first: 0,
      from_previous: 0,
      dropped: 0,
      avg_seconds: numOrNull(r.avg_seconds),
      median_seconds: numOrNull(r.median_seconds),
    });
  }
  for (const g of groups.values()) {
    g.steps.sort((a, b) => a.step - b.step);
    const first = g.steps[0]?.people ?? 0;
    g.steps.forEach((s, i) => {
      const prev = i === 0 ? s.people : g.steps[i - 1].people;
      s.from_first = first ? s.people / first : 0;
      s.from_previous = prev ? s.people / prev : 0;
      s.dropped = i === 0 ? 0 : prev - s.people;
    });
  }
  if (groups.size === 0) {
    return { kind: "funnels", groups: [{ breakdown: null, breakdown_value: null, steps: q.steps.map((s, i) => ({ step: i + 1, name: seriesLabel(s, ctx.actions), people: 0, from_first: 0, from_previous: 0, dropped: 0, avg_seconds: null, median_seconds: null })) }] };
  }
  return { kind: "funnels", groups: [...groups.values()] };
}

async function runRetention(c: Client, q: RetentionQuery, range: ResolvedRange, ctx: BuildContext): Promise<RetentionResult> {
  const n = q.intervals ?? 8;
  const rows = await run(c, buildRetention(q, range, ctx));
  const period = q.period ?? "day";
  return {
    kind: "retention",
    period,
    cohorts: rows.map((r, row) => {
      const size = num(r.size);
      const returned = (r.returned ?? {}) as Record<string, number>;
      const cols = Math.max(1, n - row);
      return { label: period === "month" ? String(r.cohort).slice(0, 7) : String(r.cohort).slice(0, 10), size, values: Array.from({ length: cols }, (_, k) => (k === 0 ? size : num(returned[String(k)]))) };
    }),
  };
}

async function runPaths(c: Client, q: PathsQuery, range: ResolvedRange, ctx: BuildContext): Promise<PathsResult> {
  const rows = await run(c, buildPaths(q, range, ctx));
  return {
    kind: "paths",
    edges: rows.map((r) => ({ source_step: Number(r.source_step), source: String(r.source), target: String(r.target), people: num(r.people), avg_seconds: numOrNull(r.avg_seconds), ends: num(r.ends) })),
  };
}

async function runStickiness(c: Client, q: StickinessQuery, range: ResolvedRange, ctx: BuildContext): Promise<StickinessResult> {
  const series: StickinessResult["series"] = [];
  for (let i = 0; i < q.series.length; i += 1) {
    const rows = await run(c, buildStickiness(q, range, ctx, i));
    series.push({ label: seriesLabel(q.series[i], ctx.actions), data: rows.map((r) => num(r.people)) });
  }
  return { kind: "stickiness", interval: q.interval ?? "day", series };
}

async function runLifecycle(c: Client, q: LifecycleQuery, range: ResolvedRange, ctx: BuildContext): Promise<LifecycleResult> {
  const interval = q.interval ?? "day";
  const rows = await run(c, buildLifecycle(q, range, ctx));
  const labels: string[] = [];
  const statuses: LifecycleResult["statuses"] = { new: [], returning: [], resurrecting: [], dormant: [] };
  for (const r of rows) {
    const l = cut(String(r.bucket), interval);
    if (labels[labels.length - 1] !== l) {
      labels.push(l);
      for (const k of Object.keys(statuses) as (keyof typeof statuses)[]) statuses[k].push(0);
    }
    const st = String(r.status) as keyof typeof statuses;
    statuses[st][labels.length - 1] = num(r.people);
  }
  return { kind: "lifecycle", interval, labels, statuses };
}

export interface RunOptions {
  timeoutMs?: number;
}

async function withTimeout<T>(db: Db, projectId: string, timeoutMs: number, fn: (c: Client) => Promise<T>): Promise<T> {
  return db.asProject(projectId, async (c) => {
    await c.query(`set local statement_timeout = ${Math.max(1000, Math.floor(timeoutMs))}`);
    // Sorts and hashes for distinct counts stay in memory (see docs/analytics/CAPACITY.md).
    await c.query(`set local work_mem = '${INSIGHT_WORK_MEM}'`);
    return fn(c);
  });
}

export async function runInsight(db: Db, projectId: string, q: InsightQuery, now: Date, opts: RunOptions = {}): Promise<InsightResult> {
  const range = resolveRange(q.date_range, now);
  try {
    return await withTimeout(db, projectId, opts.timeoutMs ?? QUERY_TIMEOUT_MS, async (c) => {
      const ctx: BuildContext = { projectId, now, actions: await loadActions(c, projectId, actionIds(q)) };
      switch (q.kind) {
        case "trends":
          return runTrends(c, q, range, ctx);
        case "funnels":
          return runFunnels(c, q, range, ctx);
        case "retention":
          return runRetention(c, q, range, ctx);
        case "paths":
          return runPaths(c, q, range, ctx);
        case "stickiness":
          return runStickiness(c, q, range, ctx);
        case "lifecycle":
          return runLifecycle(c, q, range, ctx);
      }
    });
  } catch (err) {
    throw plainError(err);
  }
}

/** Database errors a user can fix become QueryErrors; others pass through. */
export function plainError(err: unknown): unknown {
  if (err instanceof QueryError) return err;
  const code = (err as { code?: string }).code;
  if (code === "57014") return new QueryError("This insight took too long. Pick a shorter range or fewer breakdown values.");
  if (code === "2201B") return new QueryError("A pattern in a filter is not a valid regular expression.");
  if (code === "22P02" || code === "22007" || code === "22008") return new QueryError("A value in the query does not fit its field. Check dates and numbers.");
  return err;
}

export interface FunnelPerson {
  id: string;
  distinct_id: string | null;
  entered_at: string;
  properties: Record<string, unknown> | null;
}

export async function runFunnelPeople(db: Db, projectId: string, q: FunnelsQuery, now: Date, step: number, dropped: boolean, breakdownValue?: string | null): Promise<FunnelPerson[]> {
  const range = resolveRange(q.date_range, now);
  try {
    return await withTimeout(db, projectId, QUERY_TIMEOUT_MS, async (c) => {
      const ctx: BuildContext = { projectId, now, actions: await loadActions(c, projectId, actionIds(q)) };
      const rows = await run(c, buildFunnelPeople(q, range, ctx, step, dropped, 100, breakdownValue));
      return rows.map((r) => ({ id: String(r.id), distinct_id: r.distinct_id == null ? null : String(r.distinct_id), entered_at: new Date(String(r.entered_at)).toISOString(), properties: (r.properties as Record<string, unknown>) ?? null }));
    });
  } catch (err) {
    throw plainError(err);
  }
}

export interface TimeToConvert {
  bins: { from_seconds: number; to_seconds: number; people: number }[];
  total: number;
}

export async function runTimeToConvert(db: Db, projectId: string, q: FunnelsQuery, now: Date, bins = 10): Promise<TimeToConvert> {
  const range = resolveRange(q.date_range, now);
  try {
    return await withTimeout(db, projectId, QUERY_TIMEOUT_MS, async (c) => {
      const ctx: BuildContext = { projectId, now, actions: await loadActions(c, projectId, actionIds(q)) };
      const rows = await run(c, buildTimeToConvert(q, range, ctx, bins));
      if (!rows.length) return { bins: [], total: 0 };
      const lo = Number(rows[0].lo);
      const hi = Number(rows[0].hi) + 1;
      const width = (hi - lo) / bins;
      const counts = new Map(rows.map((r) => [Number(r.bin), num(r.people)]));
      return {
        total: num(rows[0].total),
        bins: Array.from({ length: bins }, (_, i) => ({ from_seconds: lo + i * width, to_seconds: lo + (i + 1) * width, people: counts.get(i + 1) ?? 0 })),
      };
    });
  } catch (err) {
    throw plainError(err);
  }
}
