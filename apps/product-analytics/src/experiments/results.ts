// Experiment results. Exposure is the SDK's `$feature_flag_called` event for
// the experiment's flag with a variant response; a person counts once, in the
// variant of their exposures (people seen in two variants are left out and
// counted). The metric counts only events after the person's first exposure,
// inside the experiment's run. Each variant is compared with control by the
// experiment's method (ported stats), and the exposure split is checked
// against the planned weights (sample-ratio check).

import type { Db } from "../db.ts";
import { bayesianTest, frequentistTest, StatisticError, type Stat } from "./stats/methods.ts";
import { sampleRatioCheck, type SrmResult } from "./stats/srm.ts";
import type { Experiment } from "./store.ts";

export interface VariantRow {
  variant: string;
  exposed: number;
  converted: number;
  sum: number;
  sum_squares: number;
}

export interface Comparison {
  variant: string;
  method: "bayesian" | "frequentist";
  effect: number | null;
  ci: [number, number] | null;
  chance_to_win: number | null;
  p_value: number | null;
  significant: boolean | null;
  error: string | null;
}

export interface Results {
  experiment_id: string;
  from: string;
  to: string;
  variants: (VariantRow & { mean: number | null; planned_weight: number })[];
  multiple_variants: number;
  srm: SrmResult & { warning: string | null };
  comparisons: Comparison[];
}

export async function variantRows(db: Db, projectId: string, exp: Experiment, from: Date, to: Date): Promise<{ rows: VariantRow[]; multiple: number }> {
  const keys = exp.variants.map((v) => v.key);
  const property = exp.metric.kind === "sum" ? exp.metric.property : null;
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ variant: string; n: string; converted: string; sum: string; sum_squares: string }>(
      `with exposure as (
         select coalesce(o.new_person_id, e.person_id) as pid, e.properties ->> '$feature_flag_response' as variant, e."timestamp" as ts
         from pa.events e
         left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
         where e.project_id = $1 and e.event = '$feature_flag_called' and e."timestamp" >= $2 and e."timestamp" < $3
           and e.properties ->> '$feature_flag' = $4 and e.properties ->> '$feature_flag_response' = any($5::text[])
       ), people as (
         select pid, min(variant) as variant, count(distinct variant) as variants, min(ts) as first_ts from exposure group by pid
       ), metric as (
         select coalesce(o.new_person_id, e.person_id) as pid, e."timestamp" as ts,
           case when $7::text is null then 1::float8
                when jsonb_typeof(e.properties -> $7::text) = 'number' then (e.properties ->> $7::text)::float8
                when (e.properties ->> $7::text) ~ '^-?[0-9]{1,15}(\\.[0-9]{1,15})?$' then (e.properties ->> $7::text)::float8
                else 0::float8 end as val
         from pa.events e
         left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
         where e.project_id = $1 and e.event = $6 and e."timestamp" >= $2 and e."timestamp" < $3
       ), per_person as (
         select p.pid, p.variant, count(m.pid) as cnt, coalesce(sum(m.val), 0) as total
         from people p left join metric m on m.pid = p.pid and m.ts >= p.first_ts
         where p.variants = 1
         group by p.pid, p.variant
       )
       select variant, count(*)::bigint as n, count(*) filter (where cnt > 0)::bigint as converted,
              coalesce(sum(total), 0)::float8 as sum, coalesce(sum(total * total), 0)::float8 as sum_squares
       from per_person group by variant`,
      [projectId, from.toISOString(), to.toISOString(), exp.flag_key, keys, exp.metric.event, property],
    );
    const m = await c.query<{ n: string }>(
      `select count(*)::bigint as n from (
         select coalesce(o.new_person_id, e.person_id) as pid
         from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
         where e.project_id = $1 and e.event = '$feature_flag_called' and e."timestamp" >= $2 and e."timestamp" < $3
           and e.properties ->> '$feature_flag' = $4 and e.properties ->> '$feature_flag_response' = any($5::text[])
         group by 1 having count(distinct e.properties ->> '$feature_flag_response') > 1) x`,
      [projectId, from.toISOString(), to.toISOString(), exp.flag_key, keys],
    );
    const byKey = new Map(r.rows.map((x) => [x.variant, x]));
    const rows = keys.map((k) => {
      const x = byKey.get(k);
      return { variant: k, exposed: Number(x?.n ?? 0), converted: Number(x?.converted ?? 0), sum: Number(x?.sum ?? 0), sum_squares: Number(x?.sum_squares ?? 0) };
    });
    return { rows, multiple: Number(m.rows[0].n) };
  });
}

function statOf(exp: Experiment, r: VariantRow): Stat {
  return exp.metric.kind === "conversion" ? { kind: "proportion", n: r.exposed, sum: r.converted } : { kind: "mean", n: r.exposed, sum: r.sum, sumSquares: r.sum_squares };
}

export function compare(exp: Experiment, rows: VariantRow[]): Comparison[] {
  const control = rows.find((r) => r.variant === "control");
  return rows
    .filter((r) => r.variant !== "control")
    .map((r): Comparison => {
      const base: Comparison = { variant: r.variant, method: exp.method, effect: null, ci: null, chance_to_win: null, p_value: null, significant: null, error: null };
      if (!control || control.exposed === 0 || r.exposed === 0) return { ...base, error: "No exposures yet in this variant or in control." };
      try {
        if (exp.method === "bayesian") {
          const b = bayesianTest(statOf(exp, r), statOf(exp, control), { difference: "relative", ciLevel: 0.95 });
          return { ...base, effect: b.effectSize, ci: b.ci, chance_to_win: b.chanceToWin, significant: b.decisive };
        }
        const f = frequentistTest(statOf(exp, r), statOf(exp, control), { difference: "relative", alpha: 0.05 });
        return { ...base, effect: f.pointEstimate, ci: f.ci, p_value: f.pValue, significant: f.significant };
      } catch (err) {
        if (err instanceof StatisticError) return { ...base, error: `Not enough data for a result yet: ${err.message}.` };
        throw err;
      }
    });
}

export async function experimentResults(db: Db, projectId: string, exp: Experiment, now: Date): Promise<Results> {
  const from = new Date(exp.started_at ?? exp.created_at);
  const to = exp.stopped_at ? new Date(exp.stopped_at) : now;
  const { rows, multiple } = await variantRows(db, projectId, exp, from, to);
  const srm = sampleRatioCheck(
    rows.map((r) => r.exposed),
    exp.variants.map((v) => v.weight),
  );
  return {
    experiment_id: exp.id,
    from: from.toISOString(),
    to: to.toISOString(),
    variants: rows.map((r) => ({
      ...r,
      planned_weight: exp.variants.find((v) => v.key === r.variant)?.weight ?? 0,
      mean: r.exposed ? (exp.metric.kind === "conversion" ? r.converted / r.exposed : r.sum / r.exposed) : null,
    })),
    multiple_variants: multiple,
    srm: { ...srm, warning: srm.mismatch ? "The split of exposed people does not match the planned weights (sample ratio mismatch). Check how the flag is read before trusting these results." : null },
    comparisons: compare(exp, rows),
  };
}
