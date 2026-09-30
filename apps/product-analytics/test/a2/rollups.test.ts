// Hourly rollups: the job's counts equal a hand count per hour; plain-count
// trends read them and stay exact with late events (inserted after the
// watermark, even for old hours), after a privacy deletion (the project is
// rebuilt) and after retention trims old hours.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { cohortsGdprHook } from "../../src/insights/cohorts.ts";
import { runRollups } from "../../src/insights/rollups.ts";
import { runInsight, type TrendsResult } from "../../src/insights/runner.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { MemorySiteDirectory } from "../../src/tenancy/site-directory.ts";
import { makeSite, skipNoPg } from "../a1/helpers.ts";
import { fixtureDb, NOW, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb(200, 60);
});

const q = (series: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
  parseQuery({ kind: "trends", date_range: { date_from: "-30d" }, series: [series], ...extra });

async function handDaily(event: string | null): Promise<number[]> {
  const rows = await f.hand<{ n: string }>(
    `select count(e.uuid) as n from generate_series('2026-08-31'::timestamptz, '2026-09-30'::timestamptz, interval '1 day') d
       left join pa.events e on e.project_id = $1 and e."timestamp" >= d and e."timestamp" < d + interval '1 day' and e."timestamp" <= $2 and ($3::text is null or e.event = $3)
      group by d order by d`,
    [f.projectId, NOW.toISOString(), event],
  );
  return rows.map((r) => Number(r.n));
}

const series = async (s: Record<string, unknown>) => ((await runInsight(f.db, f.projectId, q(s), NOW)) as TrendsResult).series[0].data;

test("the rollup job counts every hour like a hand count", { skip: skipNoPg }, async () => {
  const r = await runRollups(f.db, new Date(), { settleMs: 0, projects: [f.projectId] });
  assert.deepEqual(r.rebuilt, [f.projectId]);
  const diff = await f.hand(
    `select coalesce(a.event, b.event), coalesce(a.hour, b.hour) from pa.rollup_hourly a
       full join (select event, date_trunc('hour', "timestamp") as hour, count(*) as n from pa.events where project_id = $1 group by 1, 2) b
         on a.event = b.event and a.hour = b.hour and a.project_id = $1
      where (a.project_id = $1 or a.project_id is null) and a.count is distinct from b.n`,
    [f.projectId],
  );
  assert.deepEqual(diff, []);
  assert.deepEqual(await series({ event: "$pageview" }), await handDaily("$pageview"));
  assert.deepEqual(await series({ event: null }), await handDaily(null));
  // The answer really comes from the rollup rows: a changed row shows up, filters bypass them.
  await f.hand("update pa.rollup_hourly set count = count + 1000 where project_id = $1 and event = '$pageview' and hour = (select min(hour) from pa.rollup_hourly where project_id = $1 and event = '$pageview' and hour >= '2026-09-01')", [f.projectId]);
  const tampered = await series({ event: "$pageview" });
  assert.equal(tampered.reduce((a, b) => a + b, 0) - (await handDaily("$pageview")).reduce((a, b) => a + b, 0), 1000);
  assert.deepEqual(await series({ event: "$pageview", properties: [{ type: "event", key: "$current_url", operator: "is_set" }] }), await handDaily("$pageview"));
  await f.hand("update pa.rollup_state set reconcile_after = now() where project_id = $1", [f.projectId]);
  await runRollups(f.db, new Date(), { settleMs: 0, projects: [f.projectId] });
  assert.deepEqual(await series({ event: "$pageview" }), await handDaily("$pageview"));
});

async function handUnique(): Promise<number[]> {
  const rows = await f.hand<{ n: string }>(
    `select count(distinct coalesce(o.new_person_id, e.person_id)) as n from generate_series('2026-08-31'::timestamptz, '2026-09-30'::timestamptz, interval '1 day') d
       left join pa.events e on e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= d and e."timestamp" < d + interval '1 day' and e."timestamp" <= $2
       left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
      group by d order by d`,
    [f.projectId, NOW.toISOString()],
  );
  return rows.map((r) => Number(r.n));
}

test("unique people read the daily people rollup, through merges, with a person breakdown", { skip: skipNoPg }, async () => {
  assert.deepEqual(await series({ event: "$pageview", math: "dau" }), await handUnique());
  const r = (await runInsight(f.db, f.projectId, q({ event: "$pageview", math: "dau" }, { breakdown: { type: "person", key: "plan" } }), NOW)) as TrendsResult;
  const rows = await f.hand<{ plan: string; n: string }>(
    `select p.properties ->> 'plan' as plan, count(distinct p.id) as n from pa.events e
       left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
       join pa.persons p on p.id = coalesce(o.new_person_id, e.person_id)
      where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= '2026-08-31' and e."timestamp" <= $2 group by 1`,
    [f.projectId, NOW.toISOString()],
  );
  assert.deepEqual(new Map(r.series.map((x) => [x.breakdown, x.total])), new Map(rows.map((x) => [x.plan, Number(x.n)])));
  // The rows really come from the rollup: a removed person-day disappears from the answer.
  const [{ day, pid }] = await f.hand<{ day: string; pid: string }>("select day::text, person_id as pid from pa.rollup_daily_people where project_id = $1 and event = '$pageview' and day >= '2026-09-01' limit 1", [f.projectId]);
  await f.hand("delete from pa.rollup_daily_people where project_id = $1 and event = '$pageview' and day = $2 and person_id = $3", [f.projectId, day, pid]);
  const less = await series({ event: "$pageview", math: "dau" });
  assert.equal((await handUnique()).reduce((a, b) => a + b, 0) - less.reduce((a, b) => a + b, 0), 1);
  await f.hand("update pa.rollup_state set reconcile_after = now() where project_id = $1", [f.projectId]);
  await runRollups(f.db, new Date(), { settleMs: 0, projects: [f.projectId] });
  assert.deepEqual(await series({ event: "$pageview", math: "dau" }), await handUnique());
});

test("late events count at once, and the next run only touches their hours", { skip: skipNoPg }, async () => {
  const [{ pid }] = await f.hand<{ pid: string }>("select person_id as pid from pa.events where project_id = $1 limit 1", [f.projectId]);
  for (const at of ["2026-09-10T08:15:00Z", "2026-09-10T08:45:00Z", "2026-09-20T17:00:00Z"]) {
    await f.hand(`insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp") values ($1, $2, '$pageview', 'late', $3, $4)`, [randomUUID(), f.projectId, pid, at]);
  }
  assert.deepEqual(await series({ event: "$pageview" }), await handDaily("$pageview"), "before the next run");
  assert.deepEqual(await series({ event: "$pageview", math: "dau" }), await handUnique(), "unique people before the next run");
  const r = await runRollups(f.db, new Date(), { settleMs: 0, projects: [f.projectId] });
  assert.deepEqual(r.rebuilt, []);
  assert.equal(r.hours, 2);
  assert.deepEqual(await series({ event: "$pageview" }), await handDaily("$pageview"), "after the run");
  assert.deepEqual(await series({ event: "$pageview", math: "dau" }), await handUnique(), "unique people after the run");
});

test("a privacy deletion falls back to events until the rebuild", { skip: skipNoPg }, async () => {
  const [{ pid }] = await f.hand<{ pid: string }>("select person_id as pid from pa.events where project_id = $1 and event = '$pageview' limit 1", [f.projectId]);
  await f.hand("delete from pa.events where project_id = $1 and person_id = $2", [f.projectId, pid]);
  await cohortsGdprHook.deletePerson({ db: f.db, projectId: f.projectId, personIds: [pid], distinctIds: [], now: NOW });
  assert.deepEqual(await series({ event: "$pageview" }), await handDaily("$pageview"), "no stale rollup is read");
  const r = await runRollups(f.db, new Date(), { settleMs: 0, projects: [f.projectId] });
  assert.deepEqual(r.rebuilt, [f.projectId]);
  assert.deepEqual(await series({ event: "$pageview" }), await handDaily("$pageview"));
});

test("hours older than the project's retention are dropped", { skip: skipNoPg }, async () => {
  const site = makeSite({ project_id: f.projectId, plan: "trial" });
  await runRollups(f.db, NOW, { settleMs: 0, projects: [], sites: new MemorySiteDirectory([site]) });
  const [{ oldest }] = await f.hand<{ oldest: Date }>("select min(hour) as oldest from pa.rollup_hourly where project_id = $1", [f.projectId]);
  assert.ok(new Date(oldest).getTime() >= NOW.getTime() - 30 * 86_400_000);
});
