// Trends match hand-written SQL on the shared fixture: counts, unique people
// through merges, weekly active, property math and percentiles, breakdowns
// with "(other)", formulas, compare to previous, group counts, filters and
// time zones.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { runInsight, type TrendsResult } from "../../src/insights/runner.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { fixtureDb, NOW, round, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb();
});

const trends = async (q: Record<string, unknown>) => (await runInsight(f.db, f.projectId, parseQuery({ kind: "trends", date_range: { date_from: "-30d" }, ...q }), NOW)) as TrendsResult;
const FROM = "2026-08-31T00:00:00Z";

async function handByDay(sql: string, params: unknown[] = []): Promise<Map<string, number>> {
  const rows = await f.hand<{ d: string; v: string }>(sql, [f.projectId, FROM, NOW.toISOString(), ...params]);
  return new Map(rows.map((r) => [r.d, Number(r.v)]));
}

function asMap(labels: string[], data: number[]): Map<string, number> {
  return new Map(labels.map((l, i) => [l, data[i]] as [string, number]).filter(([, v]) => v !== 0));
}

test("total count per day equals a hand count, every day on the axis", { skip: skipNoPg }, async () => {
  const r = await trends({ series: [{ event: "$pageview" }] });
  assert.equal(r.labels.length, 31);
  assert.equal(r.labels[0], "2026-08-31");
  const hand = await handByDay(
    `select to_char("timestamp" at time zone 'UTC', 'YYYY-MM-DD') as d, count(*) as v from pa.events
      where project_id = $1 and event = '$pageview' and "timestamp" >= $2 and "timestamp" <= $3 group by 1`,
  );
  assert.ok(hand.size > 20);
  assert.deepEqual(asMap(r.labels, r.series[0].data), hand);
  assert.equal(r.series[0].total, [...hand.values()].reduce((a, b) => a + b, 0));
});

test("unique people per day resolve merged ids to one person", { skip: skipNoPg }, async () => {
  const r = await trends({ series: [{ event: "$pageview", math: "dau" }] });
  const hand = await handByDay(
    `select to_char(e."timestamp", 'YYYY-MM-DD') as d, count(distinct coalesce(o.new_person_id, e.person_id)) as v
       from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
      where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= $2 and e."timestamp" <= $3 group by 1`,
  );
  assert.deepEqual(asMap(r.labels, r.series[0].data), hand);
  const [{ n }] = await f.hand<{ n: string }>(
    `select count(distinct coalesce(o.new_person_id, e.person_id)) as n from pa.events e
       left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
      where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= $2 and e."timestamp" <= $3`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  assert.equal(r.series[0].total, Number(n), "the total is distinct over the range, not a sum of days");
  // Without the merge, the anonymous first session would count as another person.
  const [{ raw }] = await f.hand<{ raw: string }>(
    `select count(distinct person_id) as raw from pa.events where project_id = $1 and event = '$pageview' and "timestamp" >= $2 and "timestamp" <= $3`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  assert.ok(Number(raw) > Number(n));
});

test("weekly and monthly active people per day use a trailing window", { skip: skipNoPg }, async () => {
  for (const [math, days] of [
    ["weekly_active", 7],
    ["monthly_active", 30],
  ] as const) {
    const r = await trends({ series: [{ event: "$pageview", math }] });
    const hand = await handByDay(
      `select to_char(d, 'YYYY-MM-DD') as d,
              (select count(distinct coalesce(o.new_person_id, e.person_id)) from pa.events e
                 left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
                where e.project_id = $1 and e.event = '$pageview'
                  and e."timestamp" >= d - ($4::int - 1) * interval '1 day' and e."timestamp" < d + interval '1 day' and e."timestamp" <= $3) as v
         from generate_series($2::timestamptz, $3::timestamptz, interval '1 day') as d`,
      [days],
    );
    for (const [k, v] of [...hand]) if (v === 0) hand.delete(k);
    assert.deepEqual(asMap(r.labels, r.series[0].data), hand, math);
  }
  await assert.rejects(trends({ interval: "week", series: [{ event: "$pageview", math: "weekly_active" }] }), /per day/);
});

test("sum, average, min, max, median and p95 of a numeric property", { skip: skipNoPg }, async () => {
  const hand = await f.hand<Record<string, string>>(
    `select sum(v) as sum, avg(v) as avg, min(v) as min, max(v) as max,
            percentile_cont(0.5) within group (order by v) as median, percentile_cont(0.95) within group (order by v) as p95
       from (select (properties ->> 'amount')::double precision as v from pa.events
              where project_id = $1 and event = 'purchase' and "timestamp" >= $2 and "timestamp" <= $3) x`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  for (const math of ["sum", "avg", "min", "max", "median", "p95"]) {
    const r = await trends({ series: [{ event: "purchase", math, math_property: "amount" }] });
    assert.equal(round(r.series[0].total ?? NaN, 4), round(Number(hand[0][math]), 4), math);
  }
  const perDay = await trends({ series: [{ event: "purchase", math: "sum", math_property: "amount" }] });
  const handDay = await handByDay(
    `select to_char("timestamp", 'YYYY-MM-DD') as d, sum((properties ->> 'amount')::double precision) as v from pa.events
      where project_id = $1 and event = 'purchase' and "timestamp" >= $2 and "timestamp" <= $3 group by 1`,
  );
  assert.deepEqual(
    new Map([...asMap(perDay.labels, perDay.series[0].data)].map(([k, v]) => [k, round(v, 4)])),
    new Map([...handDay].map(([k, v]) => [k, round(v, 4)])),
  );
  await assert.rejects(trends({ series: [{ event: "purchase", math: "sum" }] }), /numeric property/);
});

test("a person breakdown keeps the top values and sums the rest into (other)", { skip: skipNoPg }, async () => {
  const r = await trends({ series: [{ event: "$pageview", math: "dau" }], breakdown: { type: "person", key: "country", limit: 2 } });
  const hand = await f.hand<{ country: string; n: string }>(
    `select p.properties ->> 'country' as country, count(*) as n
       from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
       left join pa.persons p on p.project_id = e.project_id and p.id = coalesce(o.new_person_id, e.person_id)
      where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= $2 and e."timestamp" <= $3
      group by 1 order by 2 desc, 1 limit 2`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  const top = hand.map((h) => h.country);
  assert.deepEqual(r.series.map((s) => s.breakdown), [...top, "(other)"]);
  for (const s of r.series) {
    const cond = s.breakdown === "(other)" ? "not (p.properties ->> 'country' = any($4))" : "p.properties ->> 'country' = any($4)";
    const want = await handByDay(
      `select to_char(e."timestamp", 'YYYY-MM-DD') as d, count(distinct coalesce(o.new_person_id, e.person_id)) as v
         from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
         left join pa.persons p on p.project_id = e.project_id and p.id = coalesce(o.new_person_id, e.person_id)
        where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= $2 and e."timestamp" <= $3 and ${cond} group by 1`,
      [s.breakdown === "(other)" ? top : [s.breakdown]],
    );
    assert.deepEqual(asMap(r.labels, s.data), want, String(s.breakdown));
  }
});

test("an event breakdown labels missing values (none)", { skip: skipNoPg }, async () => {
  const r = await trends({ series: [{ event: "signup" }], breakdown: { type: "event", key: "source" } });
  const hand = await f.hand<{ source: string | null; n: string }>(
    `select properties ->> 'source' as source, count(*) as n from pa.events
      where project_id = $1 and event = 'signup' and "timestamp" >= $2 and "timestamp" <= $3 group by 1`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  const got = new Map(r.series.map((s) => [s.breakdown, s.total]));
  assert.deepEqual(got, new Map(hand.map((h) => [h.source ?? "(none)", Number(h.n)])));
});

test("a formula divides series per day; division by zero is 0", { skip: skipNoPg }, async () => {
  const r = await trends({ series: [{ event: "signup" }, { event: "$pageview" }], formula: "A / B * 100" });
  assert.equal(r.series.length, 1);
  const hand = await handByDay(
    `select to_char("timestamp", 'YYYY-MM-DD') as d,
            case when count(*) filter (where event = '$pageview') = 0 then 0
                 else count(*) filter (where event = 'signup')::double precision / count(*) filter (where event = '$pageview') * 100 end as v
       from pa.events where project_id = $1 and event in ('signup', '$pageview') and "timestamp" >= $2 and "timestamp" <= $3 group by 1`,
  );
  const got = asMap(r.labels, r.series[0].data);
  assert.deepEqual(new Map([...got].map(([k, v]) => [k, round(v)])), new Map([...hand].filter(([, v]) => v !== 0).map(([k, v]) => [k, round(v)])));
  await assert.rejects(trends({ series: [{ event: "signup" }], formula: "A / C" }), /only 1 series/);
  await assert.rejects(trends({ series: [{ event: "signup" }], formula: "A; drop table x" }), /series letters/);
});

test("compare to previous runs the same length right before", { skip: skipNoPg }, async () => {
  const r = await trends({ date_range: { date_from: "2026-09-10", date_to: "2026-09-19" }, series: [{ event: "$pageview" }], compare: true });
  assert.deepEqual(r.labels, Array.from({ length: 10 }, (_, i) => `2026-09-${String(10 + i).padStart(2, "0")}`));
  assert.ok(r.previous);
  assert.equal(r.previous.labels[0], "2026-08-31");
  assert.equal(r.previous.labels.length, 10);
  const [{ n }] = await f.hand<{ n: string }>(
    `select count(*) as n from pa.events where project_id = $1 and event = '$pageview' and "timestamp" >= '2026-08-31' and "timestamp" < '2026-09-10'`,
    [f.projectId],
  );
  assert.equal(r.previous.series[0].total, Number(n));
});

test("unique groups, and event, person, group and numeric filters", { skip: skipNoPg }, async () => {
  const g = await trends({ series: [{ event: "$pageview", math: "unique_group", math_group_type_index: 0 }] });
  const hand = await handByDay(
    `select to_char("timestamp", 'YYYY-MM-DD') as d, count(distinct "$group_0") as v from pa.events
      where project_id = $1 and event = '$pageview' and "timestamp" >= $2 and "timestamp" <= $3 and "$group_0" <> '' group by 1`,
  );
  assert.deepEqual(asMap(g.labels, g.series[0].data), hand);

  const filtered = await trends({
    series: [{ event: "$pageview", properties: [{ type: "event", key: "$current_url", operator: "icontains", value: "PRICING" }] }],
    properties: [
      { type: "person", key: "plan", operator: "exact", value: ["pro", "team"] },
      { type: "group", key: "industry", operator: "exact", value: "retail", group_type_index: 0 },
    ],
  });
  const [{ n }] = await f.hand<{ n: string }>(
    `select count(*) as n from pa.events e
       left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
       join pa.persons p on p.project_id = e.project_id and p.id = coalesce(o.new_person_id, e.person_id)
       join pa.groups g on g.project_id = e.project_id and g.group_type_index = 0 and g.group_key = e."$group_0"
      where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= $2 and e."timestamp" <= $3
        and e.properties ->> '$current_url' ilike '%pricing%' and p.properties ->> 'plan' in ('pro', 'team') and g.properties ->> 'industry' = 'retail'`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  assert.ok(Number(n) > 0);
  assert.equal(filtered.series[0].total, Number(n));

  const big = await trends({ series: [{ event: "purchase", properties: [{ type: "event", key: "amount", operator: "gt", value: 100 }] }] });
  const [{ m }] = await f.hand<{ m: string }>(
    `select count(*) as m from pa.events where project_id = $1 and event = 'purchase' and "timestamp" >= $2 and "timestamp" <= $3 and (properties ->> 'amount')::numeric > 100`,
    [f.projectId, FROM, NOW.toISOString()],
  );
  assert.equal(big.series[0].total, Number(m));
});

test("buckets follow the time zone; hours and weeks line up", { skip: skipNoPg }, async () => {
  const r = await trends({ series: [{ event: "$pageview" }], timezone: "America/New_York" });
  const hand = await handByDay(
    `select to_char("timestamp" at time zone 'America/New_York', 'YYYY-MM-DD') as d, count(*) as v from pa.events
      where project_id = $1 and event = '$pageview'
        and "timestamp" >= (date_trunc('day', $2::timestamptz at time zone 'America/New_York') at time zone 'America/New_York') and "timestamp" <= $3 group by 1`,
  );
  assert.deepEqual(asMap(r.labels, r.series[0].data), hand);
  const w = await trends({ interval: "week", series: [{ event: "$pageview" }] });
  assert.ok(w.labels.every((l) => new Date(`${l}T00:00:00Z`).getUTCDay() === 1), "weeks start on Monday");
  const h = await trends({ interval: "hour", date_range: { date_from: "-24h" }, series: [{ event: "$pageview" }] });
  assert.equal(h.labels.length, 25);
  assert.match(h.labels[0], /^\d{4}-\d{2}-\d{2} \d{2}:00$/);
  await assert.rejects(trends({ series: [{ event: "$pageview" }], timezone: "Mars/Base" }), /time zone/);
});

test("another project's events never show up", { skip: skipNoPg }, async () => {
  const mine = await trends({ series: [{ event: null }] });
  const [{ n }] = await f.hand<{ n: string }>(`select count(*) as n from pa.events where project_id = $1 and "timestamp" >= $2 and "timestamp" <= $3`, [f.projectId, FROM, NOW.toISOString()]);
  assert.equal(mine.series[0].total, Number(n));
  const other = (await runInsight(f.db, f.otherProjectId, parseQuery({ kind: "trends", date_range: { date_from: "-30d" }, series: [{ event: null }] }), NOW)) as TrendsResult;
  assert.notEqual(other.series[0].total, mine.series[0].total);
});
