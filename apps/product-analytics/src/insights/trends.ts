// Trends: one statement per series. Semantics from the upstream trends
// runner (MIT, products/product_analytics/backend/hogql_queries/trends,
// commit cbb1e9253027): math total / dau (unique people per interval) /
// weekly_active and monthly_active (people active in the 7 or 30 days
// ending on each day) / unique_group / sum, avg, min, max, median and
// percentiles of a numeric property; a breakdown keeps its top values and
// sums the rest into "(other)"; a formula combines series A, B, ... per
// interval; compare runs the same range right before. The bucketed Postgres
// shape follows the vendored event-metrics SQL (ported/umami).

import { breakdownValue, filtersSql, Joins, seriesSql } from "./filters.ts";
import { bucketExpr, INTERVAL_SQL, Params, PERSON, PERSON_JOIN, wholeHourZone, type ResolvedRange } from "./sql.ts";
import { PROPERTY_MATH, QueryError, type BuildContext, type Built, type Interval, type Series, type TrendsQuery } from "./types.ts";

export const DEFAULT_BREAKDOWN_LIMIT = 10;
export const MAX_BREAKDOWN_LIMIT = 25;

const AGG: Record<string, string> = {
  total: "count(*)::double precision",
  dau: "count(distinct pid)::double precision",
  unique_group: "count(distinct pid)::double precision",
  sum: "sum(mv)",
  avg: "avg(mv)",
  min: "min(mv)",
  max: "max(mv)",
  median: "percentile_cont(0.5) within group (order by mv)",
  p75: "percentile_cont(0.75) within group (order by mv)",
  p90: "percentile_cont(0.90) within group (order by mv)",
  p95: "percentile_cont(0.95) within group (order by mv)",
  p99: "percentile_cont(0.99) within group (order by mv)",
};

export const LABEL_FORMAT = "'YYYY-MM-DD HH24:MI'";

/** The axis of local bucket starts for a range. */
export function axisSql(interval: Interval, P: Params, from: string, to: string, tz: string): string {
  return `select generate_series(${bucketExpr(`${from}::timestamptz`, interval, tz)}, ${bucketExpr(`${to}::timestamptz`, interval, tz)}, interval '${INTERVAL_SQL[interval]}') as bucket`;
}

/**
 * One series of a trends query. Rows: (bucket text, bk, bv, value, total)
 * where bk is 0 for a missing value, 1 for a kept value, 2 for "(other)".
 */
export function buildTrendSeries(q: TrendsQuery, s: Series, range: ResolvedRange, ctx: BuildContext): Built {
  const interval = q.interval ?? "day";
  const math = s.math ?? "total";
  if ((math === "weekly_active" || math === "monthly_active") && interval !== "day") {
    throw new QueryError("Weekly and monthly active people are shown per day. Switch the interval to day.");
  }
  if (PROPERTY_MATH.includes(math) && !s.math_property) throw new QueryError("Pick the numeric property to add up.");
  const P = new Params();
  const J = new Joins();
  const proj = P.add(ctx.projectId);
  const tz = P.add(q.timezone ?? "UTC");
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  const lookback = math === "weekly_active" ? "6 days" : math === "monthly_active" ? "29 days" : null;
  const bucketFrom = `(${bucketExpr(`${from}::timestamptz`, interval, tz)} at time zone ${tz})`;
  const where = [
    `e.project_id = ${proj}`,
    lookback ? `e."timestamp" >= ${bucketFrom} - interval '${lookback}'` : `e."timestamp" >= ${bucketFrom}`,
    `e."timestamp" <= ${to}::timestamptz`,
    seriesSql(s, ctx, P, J, proj),
    filtersSql(q.properties, P, J, proj),
  ];
  let actor = PERSON;
  if (math === "unique_group") {
    const g = s.math_group_type_index;
    if (g === undefined || !Number.isInteger(g) || g < 0 || g > 4) throw new QueryError("Pick the group type to count.");
    actor = `e."$group_${g}"`;
    where.push(`coalesce(e."$group_${g}", '') <> ''`);
  }
  const mv = PROPERTY_MATH.includes(math) ? numberOf(`e.properties ->> ${P.add(s.math_property)}`) : "null::double precision";
  const bv = q.breakdown ? breakdownValue(q.breakdown, P, J) : "null::text";
  const limit = P.add(Math.min(MAX_BREAKDOWN_LIMIT, Math.max(1, q.breakdown?.limit ?? DEFAULT_BREAKDOWN_LIMIT)));
  const ev = `select ${bucketExpr('e."timestamp"', interval, tz)} as bucket, e."timestamp" at time zone ${tz} as lt, ${bv} as bv, ${actor} as pid, ${mv} as mv
      from pa.events e
      ${PERSON_JOIN}
      ${J.sql()}
     where ${where.join("\n       and ")}`;
  const rolling = lookback !== null;
  const agg = AGG[rolling ? "dau" : math];
  const inRange = rolling ? `where ev.lt >= ${bucketExpr(`${from}::timestamptz`, interval, tz)}` : "";
  const sql = `with axis as (${axisSql(interval, P, from, to, tz)}),
ev as materialized (${ev}),
top as (select bv from ev ${inRange} group by bv order by count(*) desc, bv nulls last limit ${limit}),
kept as (
  select ev.bucket, ev.lt, ev.pid, ev.mv,
         case when exists (select 1 from top where top.bv is not distinct from ev.bv) then (case when ev.bv is null then 0 else 1 end) else 2 end as bk,
         case when exists (select 1 from top where top.bv is not distinct from ev.bv) then ev.bv end as bv
    from ev
),
${
  rolling
    ? `agg as (
  select a.bucket, k.bk, k.bv, ${agg} as v
    from axis a join kept k on k.lt >= a.bucket - interval '${lookback}' and k.lt < a.bucket + interval '1 day'
   group by a.bucket, k.bk, k.bv
),
tot as (select bk, bv, null::double precision as v, count(*) as n from kept where kept.lt >= (select min(bucket) from axis) group by bk, bv)`
    : `agg as (select bucket, bk, bv, ${agg} as v from kept group by bucket, bk, bv),
tot as (select bk, bv, ${agg} as v, count(*) as n from kept group by bk, bv)`
}
select to_char(a.bucket, ${LABEL_FORMAT}) as bucket, t.bk, t.bv, coalesce(agg.v, 0) as value, t.v as total
  from axis a
 cross join tot t
  left join agg on agg.bucket = a.bucket and agg.bk = t.bk and agg.bv is not distinct from t.bv
 order by t.bk = 2, t.n desc, t.bv nulls last, a.bucket`;
  return { sql, params: P.values };
}

export function numberOf(text: string): string {
  return `(case when (${text}) ~ '^\\s*-?[0-9]+(\\.[0-9]+)?([eE][-+]?[0-9]+)?\\s*$' then (${text})::double precision end)`;
}

/** The axis labels alone (a series with no events still has every bucket). */
export function buildAxis(q: { interval?: Interval; timezone?: string }, range: ResolvedRange): Built {
  const P = new Params();
  const tz = P.add(q.timezone ?? "UTC");
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  return { sql: `select to_char(bucket, ${LABEL_FORMAT}) as bucket from (${axisSql(q.interval ?? "day", P, from, to, tz)}) a order by bucket`, params: P.values };
}

/**
 * Plain counts (math total, an event or every event, no filters, no
 * breakdown) in a whole-hour time zone can read hourly rollups.
 */
export function rollupEligible(q: TrendsQuery, s: Series, range: ResolvedRange): boolean {
  return (
    (s.math ?? "total") === "total" &&
    s.kind === "event" &&
    !s.properties?.length &&
    !q.properties?.length &&
    !q.breakdown &&
    q.interval !== undefined &&
    q.interval !== "hour" &&
    wholeHourZone(q.timezone ?? "UTC", range)
  );
}

/**
 * The rollup form of a plain count: rollup rows for whole hours in
 * [bucket start, hour of date_to), and events for the rest (after that hour,
 * and anything inserted since the rollup watermark `rolledTo`). Same rows as
 * buildTrendSeries.
 */
export function buildTrendRollup(q: TrendsQuery, s: Series, range: ResolvedRange, ctx: BuildContext, rolledTo: Date): Built {
  const interval = q.interval ?? "day";
  const P = new Params();
  const proj = P.add(ctx.projectId);
  const tz = P.add(q.timezone ?? "UTC");
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  const hourTo = P.add(new Date(Math.floor(range.to.getTime() / 3600_000) * 3600_000).toISOString());
  const w = P.add(rolledTo.toISOString());
  const hFrom = `(${bucketExpr(`${from}::timestamptz`, interval, tz)} at time zone ${tz})`;
  const ev = s.event ? P.add(s.event) : null;
  const evR = ev ? `and r.event = ${ev}` : "";
  const evE = ev ? `and e.event = ${ev}` : "";
  const b = (col: string) => bucketExpr(col, interval, tz);
  const sql = `with axis as (${axisSql(interval, P, from, to, tz)}),
parts as (
  select ${b("r.hour")} as bucket, r.count as n from pa.rollup_hourly r
   where r.project_id = ${proj} ${evR} and r.hour >= ${hFrom} and r.hour < ${hourTo}::timestamptz
  union all
  select ${b('e."timestamp"')}, 1 from pa.events e
   where e.project_id = ${proj} ${evE} and e."timestamp" >= greatest(${hourTo}::timestamptz, ${hFrom}) and e."timestamp" <= ${to}::timestamptz
  union all
  select ${b('e."timestamp"')}, 1 from pa.events e
   where e.project_id = ${proj} and e.inserted_at >= ${w}::timestamptz ${evE} and e."timestamp" >= ${hFrom} and e."timestamp" < ${hourTo}::timestamptz
),
agg as (select bucket, sum(n)::double precision as v from parts group by bucket),
tot as (select sum(v) as v from agg)
select to_char(a.bucket, ${LABEL_FORMAT}) as bucket, 0 as bk, null::text as bv, coalesce(agg.v, 0) as value, tot.v as total
  from axis a cross join tot left join agg on agg.bucket = a.bucket
 where tot.v is not null
 order by a.bucket`;
  return { sql, params: P.values };
}
