// Retention grid. The cohort_items / user_activities / cohort_size shape is
// ported from the vendored retention SQL (ported/umami/getRetention.ts.upstream,
// MIT), keyed by person instead of session. Semantics from the upstream
// retention runner (MIT, products/product_analytics/backend/hogql_queries/
// retention, commit cbb1e9253027):
//   first_time  a person belongs to the cohort of the period of their first
//               ever start event, if that period is in the range;
//   recurring   a person belongs to the cohort of every period in which they
//               did the start event;
//   period 0    is the cohort size; period k counts cohort members who did
//               the return event in the k-th period after their cohort.
// Rows: `intervals` cohorts, the last one ending at date_to; columns 0 ..
// (intervals - 1 - row).

import { filtersSql, Joins, seriesSql } from "./filters.ts";
import { bucketExpr, Params, PERSON, PERSON_JOIN, type ResolvedRange } from "./sql.ts";
import { LABEL_FORMAT } from "./trends.ts";
import { QueryError, type BuildContext, type Built, type RetentionQuery } from "./types.ts";

export const MAX_INTERVALS = 31;

export function buildRetention(q: RetentionQuery, range: ResolvedRange, ctx: BuildContext): Built {
  const period = q.period ?? "day";
  const n = q.intervals ?? 8;
  if (!Number.isInteger(n) || n < 1 || n > MAX_INTERVALS) throw new QueryError(`Show 1 to ${MAX_INTERVALS} periods.`);
  const type = q.retention_type ?? "first_time";
  const P = new Params();
  const J = new Joins();
  const proj = P.add(ctx.projectId);
  const tz = P.add(q.timezone ?? "UTC");
  const nParam = P.add(n);
  const to = P.add(range.to.toISOString());
  const startCond = seriesSql(q.start, ctx, P, J, proj);
  const returnCond = seriesSql(q.return, ctx, P, J, proj);
  const global = filtersSql(q.properties, P, J, proj);
  const bucket = bucketExpr('e."timestamp"', period, tz);
  const periodIndex = (col: string, base: string) =>
    period === "month"
      ? `((extract(year from ${col}) - extract(year from ${base})) * 12 + (extract(month from ${col}) - extract(month from ${base})))::int`
      : period === "week"
        ? `(extract(epoch from (${col} - ${base})) / 604800)::int`
        : `(extract(epoch from (${col} - ${base})) / 86400)::int`;
  // The first cohort: (intervals - 1) periods before the period holding date_to.
  const firstBucket = `(${bucketExpr(`${to}::timestamptz`, period, tz)} - (${nParam}::int - 1) * interval '1 ${period}')`;
  const from = `(${firstBucket} at time zone ${tz})`;
  // First-time cohorts look at all history for the first start event; recurring ones only at the range.
  const startWhere = type === "first_time" ? `e.project_id = ${proj} and e."timestamp" <= ${to}::timestamptz` : `e.project_id = ${proj} and e."timestamp" >= ${from} and e."timestamp" <= ${to}::timestamptz`;
  const cohortItems =
    type === "first_time"
      ? `select pid, min(bucket) as cohort from starts group by pid having min(bucket) >= ${firstBucket}`
      : `select distinct pid, bucket as cohort from starts`;
  const sql = `with starts as materialized (
  select ${PERSON} as pid, ${bucket} as bucket
    from pa.events e
    ${PERSON_JOIN}
    ${J.sql()}
   where ${startWhere} and ${startCond} and ${global}
),
cohort_items as (${cohortItems}),
returns as materialized (
  select distinct ${PERSON} as pid, ${bucket} as bucket
    from pa.events e
    ${PERSON_JOIN}
    ${J.sql()}
   where e.project_id = ${proj} and e."timestamp" >= ${from} and e."timestamp" <= ${to}::timestamptz
     and ${returnCond} and ${global}
),
user_activities as (
  select c.pid, c.cohort, ${periodIndex("r.bucket", "c.cohort")} as k
    from cohort_items c join returns r on r.pid = c.pid and r.bucket > c.cohort
),
cohort_size as (select cohort, count(*) as people from cohort_items group by cohort),
cohort_period as (select cohort, k, count(distinct pid) as people from user_activities where k between 1 and ${nParam}::int - 1 group by cohort, k),
axis as (
  select generate_series(${firstBucket}, ${bucketExpr(`${to}::timestamptz`, period, tz)}, interval '1 ${period}') as cohort
)
select to_char(a.cohort, ${LABEL_FORMAT}) as cohort, row_number() over (order by a.cohort) - 1 as row_index, coalesce(s.people, 0) as size,
       coalesce((select jsonb_object_agg(cp.k, cp.people) from cohort_period cp where cp.cohort = a.cohort), '{}'::jsonb) as returned
  from axis a
  left join cohort_size s on s.cohort = a.cohort
 order by a.cohort`;
  return { sql, params: P.values };
}
