// Stickiness and lifecycle. Semantics ported from the upstream runners
// (MIT, products/product_analytics/backend/hogql_queries/{stickiness,lifecycle},
// commit cbb1e9253027):
//   stickiness  for n = 1 .. intervals in the range, the people active on
//               exactly n intervals;
//   lifecycle   per person, sorted active periods; a period is "new" when it
//               is the period the person was created in, "returning" when the
//               person was active the period before (and it is not their
//               first active period), otherwise "resurrecting"; the period
//               after an active one with no activity is "dormant" (shown as a
//               negative count). Events are read from one period before the
//               range to one after it, and people without a profile are left
//               out, as upstream does.

import { filtersSql, Joins, seriesSql } from "./filters.ts";
import { bucketExpr, INTERVAL_SQL, Params, PERSON, PERSON_JOIN, type ResolvedRange } from "./sql.ts";
import { axisSql, LABEL_FORMAT } from "./trends.ts";
import { QueryError, type BuildContext, type Built, type LifecycleQuery, type StickinessQuery } from "./types.ts";

/** Rows: (series index, n, people) for n = 1 .. number of intervals. */
export function buildStickiness(q: StickinessQuery, range: ResolvedRange, ctx: BuildContext, seriesIndex: number): Built {
  const interval = q.interval ?? "day";
  const s = q.series[seriesIndex];
  if (!s) throw new QueryError("Add a series.");
  const P = new Params();
  const J = new Joins();
  const proj = P.add(ctx.projectId);
  const tz = P.add(q.timezone ?? "UTC");
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  const bucketFrom = `(${bucketExpr(`${from}::timestamptz`, interval, tz)} at time zone ${tz})`;
  const sql = `with axis as (${axisSql(interval, P, from, to, tz)}),
active as (
  select ${PERSON} as pid, count(distinct ${bucketExpr('e."timestamp"', interval, tz)}) as n
    from pa.events e
    ${PERSON_JOIN}
    ${(() => {
      const cond = seriesSql(s, ctx, P, J, proj);
      const global = filtersSql(q.properties, P, J, proj);
      return `${J.sql()}
   where e.project_id = ${proj} and e."timestamp" >= ${bucketFrom} and e."timestamp" <= ${to}::timestamptz and ${cond} and ${global}`;
    })()}
   group by 1
),
counts as (select n, count(*) as people from active group by n)
select x.n::int as n, coalesce(c.people, 0) as people
  from generate_series(1, (select count(*) from axis)) as x(n)
  left join counts c on c.n = x.n
 order by x.n`;
  return { sql, params: P.values };
}

/** Rows: (bucket text, status, people) with dormant as a negative number; every bucket and status present. */
export function buildLifecycle(q: LifecycleQuery, range: ResolvedRange, ctx: BuildContext): Built {
  const interval = q.interval ?? "day";
  const s = q.series[0];
  if (!s) throw new QueryError("Pick the event that counts as active.");
  const P = new Params();
  const J = new Joins();
  const proj = P.add(ctx.projectId);
  const tz = P.add(q.timezone ?? "UTC");
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  const one = `interval '${INTERVAL_SQL[interval]}'`;
  const firstB = bucketExpr(`${from}::timestamptz`, interval, tz);
  const lastB = bucketExpr(`${to}::timestamptz`, interval, tz);
  const cond = seriesSql(s, ctx, P, J, proj);
  const global = filtersSql(q.properties, P, J, proj);
  const sql = `with axis as (${axisSql(interval, P, from, to, tz)}),
activity as (
  select distinct ${PERSON} as pid, ${bucketExpr('e."timestamp"', interval, tz)} as period
    from pa.events e
    ${PERSON_JOIN}
    ${J.sql()}
   where e.project_id = ${proj}
     and e."timestamp" >= ((${firstB} - ${one}) at time zone ${tz})
     and e."timestamp" < ((${lastB} + ${one}) at time zone ${tz})
     and ${cond} and ${global}
),
seq as (
  select a.pid, a.period,
         ${bucketExpr("pp.created_at", interval, tz)} as created,
         lag(a.period) over w as prev,
         lead(a.period) over w as next,
         row_number() over w as idx
    from activity a
    join pa.persons pp on pp.project_id = ${proj} and pp.id = a.pid
  window w as (partition by a.pid order by a.period)
),
statuses as (
  select pid, period,
         case when idx = 1 and period = created then 'new'
              when idx > 1 and prev = period - ${one} then 'returning'
              else 'resurrecting' end as status
    from seq
  union all
  select pid, period + ${one} as period, 'dormant' as status
    from seq
   where next is null or next <> period + ${one}
),
counts as (select period, status, count(distinct pid) as people from statuses group by period, status)
select to_char(a.bucket, ${LABEL_FORMAT}) as bucket, st.status,
       (case when st.status = 'dormant' then -1 else 1 end) * coalesce(c.people, 0) as people
  from axis a
 cross join (values ('new'), ('returning'), ('resurrecting'), ('dormant')) as st(status)
  left join counts c on c.period = a.bucket and c.status = st.status
 order by a.bucket, st.status`;
  return { sql, params: P.values };
}
