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

interface SeriesParts {
  P: Params;
  proj: string;
  J: Joins;
  tz: string;
  from: string;
  to: string;
  interval: Interval;
  lookback: string | null;
  where: string[];
  actor: string;
  mv: string;
  bv: string;
}

function seriesParts(q: TrendsQuery, s: Series, range: ResolvedRange, ctx: BuildContext): SeriesParts {
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
  return { P, proj, J, tz, from, to, interval, lookback, where, actor, mv, bv };
}

/**
 * The breakdown values a series keeps: the most frequent present values in
 * the range (at most the limit). Rows: (bv). Run before buildTrendSeries.
 */
export function buildBreakdownTop(q: TrendsQuery, s: Series, range: ResolvedRange, ctx: BuildContext): Built {
  if (!q.breakdown) throw new QueryError("Pick a property to break down by.");
  const x = seriesParts(q, s, range, ctx);
  const inRange = x.lookback ? `and e."timestamp" >= (${bucketExpr(`${x.from}::timestamptz`, x.interval, x.tz)} at time zone ${x.tz})` : "";
  const limit = x.P.add(Math.min(MAX_BREAKDOWN_LIMIT, Math.max(1, q.breakdown.limit ?? DEFAULT_BREAKDOWN_LIMIT)));
  const sql = `select bv from (
  select ${x.bv} as bv
    from pa.events e
    ${PERSON_JOIN}
    ${x.J.sql()}
   where ${x.where.join("\n     and ")} ${inRange}
) v where bv is not null group by bv order by count(*) desc, bv limit ${limit}`;
  return { sql, params: x.P.values };
}

/**
 * One series of a trends query. Rows: (bucket text, bk, bv, value, total)
 * where bk is 0 for a missing breakdown value, 1 for a kept value and 2
 * for "(other)". `top` is the kept values (buildBreakdownTop); a missing
 * value always keeps its own "(none)" line.
 */
/**
 * Unique people per day, week or month (UTC) for one event, with no filters
 * and at most a person-property breakdown, can read the daily people rollup.
 */
export function peopleRollupEligible(q: TrendsQuery, s: Series): boolean {
  return (
    (s.math ?? "total") === "dau" &&
    s.kind === "event" &&
    !!s.event &&
    !s.properties?.length &&
    !q.properties?.length &&
    (!q.breakdown || q.breakdown.type === "person") &&
    (q.timezone ?? "UTC") === "UTC" &&
    q.interval !== "hour"
  );
}

export function buildTrendSeries(q: TrendsQuery, s: Series, range: ResolvedRange, ctx: BuildContext, top: string[] = [], peopleRolledTo: Date | null = null): Built {
  const x = seriesParts(q, s, range, ctx);
  const { P, tz, from, to, interval, lookback } = x;
  const math = s.math ?? "total";
  const topP = q.breakdown ? P.add(top) : "";
  const bk = q.breakdown ? `case when b.bv is null then 0 when b.bv = any(${topP}::text[]) then 1 else 2 end` : "0";
  const bvKept = q.breakdown ? `case when b.bv = any(${topP}::text[]) then b.bv end` : "null::text";
  const rawWhere = (extra: string[]) => `select ${bucketExpr('e."timestamp"', interval, tz)} as bucket, e."timestamp" at time zone ${tz} as lt, ${x.bv} as bv, ${x.actor} as pid, ${x.mv} as mv
      from pa.events e
      ${PERSON_JOIN}
      ${x.J.sql()}
     where ${[...x.where, ...extra].join("\n       and ")}`;
  const raw = rawWhere([]);
  let source = raw;
  if (peopleRolledTo && peopleRollupEligible(q, s)) {
    // Whole UTC days before the day of date_to come from pa.rollup_daily_people
    // (events inserted before the watermark); the rest, and late events, from pa.events.
    const dayTo = P.add(new Date(Date.UTC(range.to.getUTCFullYear(), range.to.getUTCMonth(), range.to.getUTCDate())).toISOString());
    const w = P.add(peopleRolledTo.toISOString());
    const ev0 = P.add(s.event);
    const bvR = q.breakdown ? `(pp.properties ->> ${P.add(q.breakdown.key)})` : "null::text";
    const joinR = q.breakdown ? "left join pa.persons pp on pp.project_id = r.project_id and pp.id = coalesce(o.new_person_id, r.person_id)" : "";
    source = `select date_trunc('${interval}', r.day::timestamp) as bucket, r.day::timestamp as lt, ${bvR} as bv, coalesce(o.new_person_id, r.person_id) as pid, null::double precision as mv
      from pa.rollup_daily_people r
      left join pa.person_overrides o on o.project_id = r.project_id and o.old_person_id = r.person_id
      ${joinR}
     where r.project_id = ${x.proj} and r.event = ${ev0}
       and r.day >= (${bucketExpr(`${from}::timestamptz`, interval, tz)})::date and r.day < ${dayTo}::date
    union all
    select * from (${raw}) t1 where t1.lt >= ${dayTo}::timestamptz at time zone ${tz}
    union all
    select * from (${rawWhere([`e.inserted_at >= ${w}::timestamptz`])}) t2 where t2.lt < ${dayTo}::timestamptz at time zone ${tz}`;
  }
  const ev = `select b.bucket, b.lt, ${bk} as bk, ${bvKept} as bv, b.pid, b.mv from (
    ${source}
  ) b`;
  const final = (agg: string, tot: string) => `select to_char(a.bucket, ${LABEL_FORMAT}) as bucket, t.bk, t.bv, coalesce(agg.v, 0) as value, t.v as total
  from (${axisSql(interval, P, from, to, tz)}) a
 cross join (${tot}) t
  left join (${agg}) agg on agg.bucket = a.bucket and agg.bk = t.bk and agg.bv is not distinct from t.bv
 order by t.bk = 2, t.n desc, t.bv nulls last, a.bucket`;
  if (lookback) {
    const first = bucketExpr(`${from}::timestamptz`, interval, tz);
    const sql = `with ev as materialized (${ev})
${final(
  `select a.bucket, k.bk, k.bv, count(distinct k.pid)::double precision as v
     from (${axisSql(interval, P, from, to, tz)}) a join ev k on k.lt >= a.bucket - interval '${lookback}' and k.lt < a.bucket + interval '1 day'
    group by a.bucket, k.bk, k.bv`,
  `select bk, bv, null::double precision as v, count(*) as n from ev where lt >= ${first} group by bk, bv`,
)}`;
    return { sql, params: P.values };
  }
  const agg = AGG[math];
  if (math === "dau" || math === "unique_group") {
    // Distinct counts: deduplicate (bucket, value, actor) once with a hash, then count rows.
    const sql = `with d as materialized (select distinct bucket, bk, bv, pid from (${ev}) k),
res as (
  select bucket, bk, bv, count(*)::double precision as v, count(*) as n, 0 as whole from d group by bucket, bk, bv
  union all
  select null, bk, bv, count(*)::double precision, count(*), 1 from (select distinct bk, bv, pid from d) x group by bk, bv
)
${final("select bucket, bk, bv, v from res where whole = 0", "select bk, bv, v, n from res where whole = 1")}`;
    return { sql, params: P.values };
  }
  // One pass: per bucket and in total, with grouping sets.
  const sql = `with res as materialized (
  select bucket, bk, bv, ${agg} as v, count(*) as n, grouping(bucket) as whole
    from (${ev}) k
   group by grouping sets ((bucket, bk, bv), (bk, bv))
)
${final("select bucket, bk, bv, v from res where whole = 0", "select bk, bv, v, n from res where whole = 1")}`;
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
