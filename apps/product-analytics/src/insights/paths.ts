// Paths. The per-visit ordered-events shape is ported from the vendored
// journey SQL (ported/umami/getJourney.ts.upstream, MIT); the semantics from
// the upstream paths runner (MIT, products/product_analytics/backend/
// hogql_queries/paths, commit cbb1e9253027):
//   - a person's events split into sessions at 30 minutes of quiet;
//   - repeats of the same item in a row collapse into one;
//   - a start point trims each session to begin at its first occurrence
//     (sessions without it are left out); an end point trims it to stop at
//     the first occurrence after that (sessions without it are left out);
//     with only an end point the last `depth` items up to it are kept;
//   - at most `depth` items per session; edges "i_item -> (i+1)_item" are
//     counted per session, with the average time between them.
// Items: page views by URL, screens by name, other events by name.

import { filtersSql, Joins } from "./filters.ts";
import { Params, PERSON, PERSON_JOIN, type ResolvedRange } from "./sql.ts";
import { QueryError, type BuildContext, type Built, type PathsQuery } from "./types.ts";

export const SESSION_GAP_SECONDS = 30 * 60;
export const DEFAULT_DEPTH = 5;
export const MAX_DEPTH = 10;
export const DEFAULT_EDGES = 50;

const ITEM: Record<NonNullable<PathsQuery["path_type"]>, { cond: string; item: string }> = {
  pageview: { cond: `e.event = '$pageview'`, item: `coalesce(e.properties ->> '$current_url', '')` },
  screen: { cond: `e.event = '$screen'`, item: `coalesce(e.properties ->> '$screen_name', '')` },
  custom: { cond: `e.event not like '$%'`, item: "e.event" },
  all: {
    cond: `(e.event not like '$%' or e.event in ('$pageview', '$screen'))`,
    item: `case e.event when '$pageview' then coalesce(e.properties ->> '$current_url', '') when '$screen' then coalesce(e.properties ->> '$screen_name', '') else e.event end`,
  },
};

/** Rows: (source_step, source, target, people, avg_seconds, ends) ordered by people. */
export function buildPaths(q: PathsQuery, range: ResolvedRange, ctx: BuildContext): Built {
  const type = q.path_type ?? "pageview";
  const def = ITEM[type];
  if (!def) throw new QueryError("Pick page views, screens, custom events or all of them.");
  const depth = q.depth ?? DEFAULT_DEPTH;
  if (!Number.isInteger(depth) || depth < 2 || depth > MAX_DEPTH) throw new QueryError(`Show 2 to ${MAX_DEPTH} steps.`);
  const edges = Math.min(500, Math.max(1, q.edge_limit ?? DEFAULT_EDGES));
  const P = new Params();
  const J = new Joins();
  const proj = P.add(ctx.projectId);
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  const global = filtersSql(q.properties, P, J, proj);
  const gap = P.add(SESSION_GAP_SECONDS);
  const d = P.add(depth);
  const start = q.start_point ? P.add(q.start_point) : null;
  const end = q.end_point ? P.add(q.end_point) : null;
  const trimmed =
    start && end
      ? `bounds as (
  select pid, sid, min(i) filter (where item = ${start}) as si from numbered group by pid, sid
),
after_start as (
  select n.pid, n.sid, n.item, n.ts, n.i - b.si + 1 as i
    from numbered n join bounds b on b.pid = n.pid and b.sid = n.sid
   where b.si is not null and n.i >= b.si
),
ends as (select pid, sid, min(i) filter (where item = ${end}) as ei from after_start group by pid, sid),
kept as (
  select a.pid, a.sid, a.item, a.ts, a.i from after_start a join ends x on x.pid = a.pid and x.sid = a.sid
   where x.ei is not null and a.i <= x.ei and a.i <= ${d}::int
)`
      : start
        ? `bounds as (
  select pid, sid, min(i) filter (where item = ${start}) as si from numbered group by pid, sid
),
kept as (
  select n.pid, n.sid, n.item, n.ts, n.i - b.si + 1 as i
    from numbered n join bounds b on b.pid = n.pid and b.sid = n.sid
   where b.si is not null and n.i >= b.si and n.i - b.si + 1 <= ${d}::int
)`
        : end
          ? `bounds as (
  select pid, sid, min(i) filter (where item = ${end}) as ei from numbered group by pid, sid
),
kept as (
  select n.pid, n.sid, n.item, n.ts, n.i - greatest(b.ei - ${d}::int, 0) as i
    from numbered n join bounds b on b.pid = n.pid and b.sid = n.sid
   where b.ei is not null and n.i <= b.ei and n.i > b.ei - ${d}::int
)`
          : `kept as (select pid, sid, item, ts, i from numbered where i <= ${d}::int)`;
  const sql = `with ev as materialized (
  select ${PERSON} as pid, e."timestamp" as ts, e.uuid, ${def.item} as item
    from pa.events e
    ${PERSON_JOIN}
    ${J.sql()}
   where e.project_id = ${proj} and e."timestamp" >= ${from}::timestamptz and e."timestamp" <= ${to}::timestamptz
     and ${def.cond} and ${global}
),
marked as (
  select pid, ts, uuid, item,
         case when lag(ts) over w is null or extract(epoch from ts - lag(ts) over w) >= ${gap}::int then 1 else 0 end as brk
    from ev window w as (partition by pid order by ts, uuid)
),
sess as (
  select pid, ts, uuid, item, sum(brk) over (partition by pid order by ts, uuid rows unbounded preceding) as sid from marked
),
compact as (
  select pid, sid, ts, uuid, item from (
    select s.*, lag(item) over (partition by pid, sid order by ts, uuid) as prev from sess s
  ) x where prev is distinct from item
),
numbered as (
  select pid, sid, item, ts, row_number() over (partition by pid, sid order by ts, uuid) as i from compact
),
${trimmed},
linked as (
  select k.i, k.item, lead(k.item) over w as next_item, lead(k.ts) over w as next_ts, k.ts
    from kept k window w as (partition by k.pid, k.sid order by k.i)
),
edge as (
  select i as source_step, item as source, next_item as target, count(*) as people,
         avg(extract(epoch from next_ts - ts)) as avg_seconds
    from linked where next_item is not null group by 1, 2, 3
),
drop_off as (select i as step, item, count(*) as ends from linked where next_item is null group by 1, 2)
select e.source_step::int, e.source, e.target, e.people, e.avg_seconds,
       coalesce((select x.ends from drop_off x where x.step = e.source_step + 1 and x.item = e.target), 0) as ends
  from edge e
 order by e.people desc, e.source_step, e.source, e.target
 limit ${P.add(edges)}`;
  return { sql, params: P.values };
}
