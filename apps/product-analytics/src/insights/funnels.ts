// Funnels on Postgres. The level-by-level CTE shape is ported from the
// vendored funnel SQL (ported/umami/getFunnel.ts.upstream, MIT): level1 is
// every entry event, level k joins the person's next matching event within
// the conversion window. Semantics from the upstream funnel runners (MIT,
// products/product_analytics/backend/hogql_queries/funnels, commit
// cbb1e9253027):
//   ordered   each step after the previous one, all within the window of
//             the first step;
//   strict    each step is the person's very next event;
//   unordered steps in any order within the window of the first of them;
//   breakdown the entry event's value (first touch), top values kept;
//   people    are counted once per breakdown value at their furthest step;
//             the furthest-reaching attempt (then the earliest) is kept,
//             which also gives time to convert between steps.
// Two fixes over the vendored shape: the window is measured from the first
// step (not the previous one), and a per-person event sequence number keeps
// one event from filling two steps.

import { breakdownValue, filtersSql, Joins, seriesSql } from "./filters.ts";
import { Params, PERSON, PERSON_JOIN, WINDOW_SQL, type ResolvedRange } from "./sql.ts";
import { QueryError, type BuildContext, type Built, type FunnelsQuery } from "./types.ts";
import { DEFAULT_BREAKDOWN_LIMIT, MAX_BREAKDOWN_LIMIT } from "./trends.ts";

export const MAX_STEPS = 10;
export const DEFAULT_WINDOW = { value: 14, unit: "day" as const };

interface Core {
  P: Params;
  proj: string;
  n: number;
  /** CTE text ending with `best (pid, bv, bk, reached, t0, t1..tn)`. */
  ctes: string;
}

function core(q: FunnelsQuery, range: ResolvedRange, ctx: BuildContext): Core {
  const n = q.steps.length;
  if (n < 2 || n > MAX_STEPS) throw new QueryError(`A funnel has 2 to ${MAX_STEPS} steps.`);
  const order = q.order ?? "ordered";
  const w = q.window ?? DEFAULT_WINDOW;
  if (!Number.isInteger(w.value) || w.value < 1 || w.value > 10_000 || !(w.unit in WINDOW_SQL)) throw new QueryError("Set a conversion window of at least one minute.");
  const P = new Params();
  const J = new Joins();
  const proj = P.add(ctx.projectId);
  const from = P.add(range.from.toISOString());
  const to = P.add(range.to.toISOString());
  const win = `(${P.add(w.value)}::int * interval '1 ${WINDOW_SQL[w.unit]}')`;
  const steps = q.steps.map((s) => seriesSql(s, ctx, P, J, proj));
  const g = q.aggregation_group_type_index;
  if (g != null && (!Number.isInteger(g) || g < 0 || g > 4)) throw new QueryError("Pick the group type to count.");
  const actor = g == null ? PERSON : `e."$group_${g}"`;
  const bv = q.breakdown ? breakdownValue(q.breakdown, P, J) : "null::text";
  const limit = P.add(Math.min(MAX_BREAKDOWN_LIMIT, Math.max(1, q.breakdown?.limit ?? DEFAULT_BREAKDOWN_LIMIT)));
  const base = [`e.project_id = ${proj}`, `e."timestamp" >= ${from}::timestamptz`, `e."timestamp" <= ${to}::timestamptz`, filtersSql(q.properties, P, J, proj)];
  if (g != null) base.push(`coalesce(e."$group_${g}", '') <> ''`);
  const flags = steps.map((c, i) => `coalesce((${c}), false) as s${i + 1}`).join(",\n         ");
  // Strict funnels look at every event of an entering person; the others only at step events.
  const scope =
    order === "strict"
      ? `${actor} in (select ${actor} from pa.events e ${PERSON_JOIN} ${J.sql()} where ${base.join(" and ")} and (${steps[0]}))`
      : `(${steps.map((c) => `(${c})`).join(" or ")})`;
  const se = `se as materialized (
  select ${actor} as pid, e."timestamp" as ts, ${bv} as bv,
         ${flags},
         row_number() over (partition by ${actor} order by e."timestamp", e.uuid) as seq
    from pa.events e
    ${PERSON_JOIN}
    ${J.sql()}
   where ${base.join("\n     and ")}
     and ${scope}
)`;
  const topFrom = order === "unordered" ? `se where ${Array.from({ length: n }, (_, i) => `s${i + 1}`).join(" or ")}` : "se where s1";
  const top = `top as (select bv from (select distinct pid, bv from ${topFrom}) x group by bv order by count(*) desc, bv nulls last limit ${limit})`;
  const keep = (col: string) =>
    `case when exists (select 1 from top where top.bv is not distinct from ${col}) then (case when ${col} is null then 0 else 1 end) else 2 end as bk,
     case when exists (select 1 from top where top.bv is not distinct from ${col}) then ${col} end as bv`;
  const tcols = Array.from({ length: n }, (_, i) => `t${i + 1}`);
  let ctes: string;
  if (order === "unordered") {
    const any = Array.from({ length: n }, (_, i) => `s${i + 1}`).join(" or ");
    const doneExpr = Array.from({ length: n }, (_, i) => `(bool_or(s.s${i + 1}))::int`).join(" + ");
    // Time of the k-th distinct step done, in order of first completion inside the window.
    const firsts = Array.from({ length: n }, (_, i) => `min(s.ts) filter (where s.s${i + 1}) as f${i + 1}`).join(", ");
    const sorted = `(select array_agg(x order by x) from unnest(array[${Array.from({ length: n }, (_, i) => `r.f${i + 1}`).join(", ")}]) as x where x is not null)`;
    ctes = `${se},
${top},
anchors as (select pid, ts as t0, seq as q0, ${keep("bv")} from se where ${any}),
reach as (
  select a.pid, a.bk, a.bv, a.t0, ${doneExpr} as reached, ${firsts}
    from anchors a join se s on s.pid = a.pid and s.seq >= a.q0 and s.ts <= a.t0 + ${win}
   where (${any.replace(/s(\d+)/g, "s.s$1")})
   group by a.pid, a.bk, a.bv, a.t0
),
ranked as (select r.*, ${sorted} as ts_sorted from reach r),
best as (
  select distinct on (pid, bk, bv) pid, bk, bv, reached, t0, ${tcols.map((c, i) => `ts_sorted[${i + 1}] as ${c}`).join(", ")}
    from ranked order by pid, bk, bv, reached desc, t0
)`;
  } else {
    const levels: string[] = [`l1 as (select pid, ts as t0, seq as q1, ts as t1, ${keep("bv")} from se where s1)`];
    for (let k = 2; k <= n; k += 1) {
      const prev = `l${k - 1}`;
      const carry = ["pid", "bk", "bv", "t0", ...Array.from({ length: k - 1 }, (_, i) => `t${i + 1}`), ...Array.from({ length: k - 1 }, (_, i) => `q${i + 1}`)];
      const on =
        order === "strict"
          ? `s.pid = l.pid and s.seq = l.q${k - 1} + 1 and s.s${k} and s.ts <= l.t0 + ${win}`
          : `s.pid = l.pid and s.seq > l.q${k - 1} and s.s${k} and s.ts <= l.t0 + ${win}`;
      levels.push(
        `l${k} as (
  select ${carry.map((c) => `l.${c}`).join(", ")}, min(s.seq) as q${k}, min(s.ts) as t${k}
    from ${prev} l join se s on ${on}
   group by ${carry.map((c) => `l.${c}`).join(", ")}
)`,
      );
    }
    const joins = Array.from({ length: n - 1 }, (_, i) => {
      const k = i + 2;
      return `left join l${k} on l${k}.pid = l1.pid and l${k}.t0 = l1.t0 and l${k}.q1 = l1.q1`;
    }).join("\n    ");
    const reached = `case ${Array.from({ length: n - 1 }, (_, i) => `when l${n - i}.pid is not null then ${n - i}`).join(" ")} else 1 end`;
    ctes = `${se},
${top},
${levels.join(",\n")},
cand as (
  select l1.pid, l1.bk, l1.bv, l1.t0, ${reached} as reached, l1.t1, ${Array.from({ length: n - 1 }, (_, i) => `l${i + 2}.t${i + 2}`).join(", ")}
    from l1
    ${joins}
),
best as (select distinct on (pid, bk, bv) * from cand order by pid, bk, bv, reached desc, t0)`;
  }
  return { P, proj, n, ctes };
}

/** Rows: (bk, bv, step, people, avg_seconds, median_seconds) for step 1..n; avg/median are from the previous step. */
export function buildFunnel(q: FunnelsQuery, range: ResolvedRange, ctx: BuildContext): Built {
  const { P, n, ctes } = core(q, range, ctx);
  const sql = `with ${ctes},
steps as (select generate_series(1, ${n}) as step),
groups as (select bk, bv, count(*) as entered from best group by bk, bv)
select g.bk, g.bv, st.step,
       count(b.pid) as people,
       avg(extract(epoch from (${stepDiff(n)}))) as avg_seconds,
       percentile_cont(0.5) within group (order by extract(epoch from (${stepDiff(n)}))) as median_seconds
  from groups g
 cross join steps st
  left join best b on b.bk = g.bk and b.bv is not distinct from g.bv and b.reached >= st.step
 group by g.bk, g.bv, g.entered, st.step
 order by g.bk = 2, g.entered desc, g.bv nulls last, st.step`;
  return { sql, params: P.values };
}

function stepDiff(n: number): string {
  // Time from the previous step to this one (null for step 1).
  const cases = Array.from({ length: n - 1 }, (_, i) => `when st.step = ${i + 2} then b.t${i + 2} - b.t${i + 1}`).join(" ");
  return `case ${cases} end`;
}

/** People who reached `step` (converted) or stopped right before it (dropped), newest first, at most `limit`. */
export function buildFunnelPeople(q: FunnelsQuery, range: ResolvedRange, ctx: BuildContext, step: number, dropped: boolean, limit = 100, breakdownValue?: string | null): Built {
  const { P, proj, n, ctes } = core(q, range, ctx);
  if (!Number.isInteger(step) || step < 1 || step > n) throw new QueryError(`Pick a step from 1 to ${n}.`);
  if (dropped && step === 1) throw new QueryError("Nobody drops off before the first step. Pick a later step.");
  const cond = dropped ? `b.reached = ${P.add(step - 1)}` : `b.reached >= ${P.add(step)}`;
  const bvCond = breakdownValue === undefined ? "true" : breakdownValue === null ? "b.bv is null" : `b.bv = ${P.add(breakdownValue)}`;
  const person = q.aggregation_group_type_index == null;
  const sql = person
    ? `with ${ctes}
select b.pid as id, max(b.t0) as entered_at, min(d.distinct_id) as distinct_id, max(p.properties::text)::jsonb as properties
  from best b
  left join pa.persons p on p.project_id = ${proj} and p.id = b.pid
  left join pa.person_distinct_ids d on d.project_id = ${proj} and d.person_id = b.pid
 where ${cond} and ${bvCond}
 group by b.pid
 order by max(b.t0) desc
 limit ${P.add(Math.min(1000, Math.max(1, limit)))}`
    : `with ${ctes}
select b.pid as id, max(b.t0) as entered_at, b.pid as distinct_id, null::jsonb as properties
  from best b
 where ${cond} and ${bvCond}
 group by b.pid
 order by max(b.t0) desc
 limit ${P.add(Math.min(1000, Math.max(1, limit)))}`;
  return { sql, params: P.values };
}

/** Time from the first to the last step for everyone who converted, in `bins` equal-width bins. */
export function buildTimeToConvert(q: FunnelsQuery, range: ResolvedRange, ctx: BuildContext, bins = 10): Built {
  const { P, n, ctes } = core(q, range, ctx);
  const b = P.add(Math.min(60, Math.max(1, bins)));
  const sql = `with ${ctes},
conv as (select extract(epoch from (t${n} - t1))::double precision as secs from best where reached = ${n}),
lim as (select min(secs) as lo, max(secs) as hi, count(*) as total from conv)
select least(width_bucket(c.secs, l.lo, l.hi + 1, ${b}), ${b}) as bin, l.lo, l.hi, l.total, count(*) as people
  from conv c cross join lim l
 group by 1, l.lo, l.hi, l.total
 order by 1`;
  return { sql, params: P.values };
}
