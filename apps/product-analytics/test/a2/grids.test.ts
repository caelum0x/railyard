// Retention (first time and recurring; day, week and month), stickiness,
// lifecycle (new, returning, resurrecting, dormant) and paths (from, to,
// depth) match hand-written SQL on the shared fixture, plus exact cases
// re-expressed from the upstream runner tests.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { runInsight, type LifecycleResult, type PathsResult, type RetentionResult, type StickinessResult } from "../../src/insights/runner.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { insertEvents } from "./fixtures.ts";
import { fixtureDb, NOW, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
const exact = randomUUID();
const A = randomUUID();
const B = randomUUID();
const C = randomUUID();

before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb();
  const d = (day: number, h = 12) => new Date(Date.UTC(2026, 8, day, h)).toISOString();
  await insertEvents(f.owner, exact, [
    // A: created and active on the 1st, back on the 2nd and the 4th.
    { person: A, event: "open", at: d(1), created: d(1, 0) },
    { person: A, event: "open", at: d(2) },
    { person: A, event: "open", at: d(4) },
    // B: created long before, active on the 2nd and 3rd.
    { person: B, event: "open", at: d(2), created: "2026-06-01T00:00:00Z" },
    { person: B, event: "open", at: d(3) },
    // C: created on the 3rd, active on the 3rd only.
    { person: C, event: "open", at: d(3), created: d(3, 0) },
    // Paths: A visits a -> b -> b -> c in one session, then d after an hour's quiet.
    { person: A, event: "$pageview", at: "2026-09-01T10:00:00Z", properties: { $current_url: "/a" } },
    { person: A, event: "$pageview", at: "2026-09-01T10:01:00Z", properties: { $current_url: "/b" } },
    { person: A, event: "$pageview", at: "2026-09-01T10:02:00Z", properties: { $current_url: "/b" } },
    { person: A, event: "$pageview", at: "2026-09-01T10:04:00Z", properties: { $current_url: "/c" } },
    { person: A, event: "$pageview", at: "2026-09-01T11:10:00Z", properties: { $current_url: "/d" } },
    { person: B, event: "$pageview", at: "2026-09-02T10:00:00Z", properties: { $current_url: "/a" } },
    { person: B, event: "$pageview", at: "2026-09-02T10:02:00Z", properties: { $current_url: "/c" } },
  ]);
});

const run = async <T>(projectId: string, q: Record<string, unknown>) => (await runInsight(f.db, projectId, parseQuery(q), NOW)) as T;
const RESOLVED = `select coalesce(o.new_person_id, e.person_id) as pid, e.uuid, e."timestamp" as ts, e.event, e.properties
                    from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
                   where e.project_id = $1`;

test("lifecycle: new, returning, resurrecting and dormant on exact days", { skip: skipNoPg }, async () => {
  const r = await run<LifecycleResult>(exact, { kind: "lifecycle", date_range: { date_from: "2026-09-01", date_to: "2026-09-05" }, series: [{ event: "open" }] });
  assert.deepEqual(r.labels, ["2026-09-01", "2026-09-02", "2026-09-03", "2026-09-04", "2026-09-05"]);
  assert.deepEqual(r.statuses.new, [1, 0, 1, 0, 0]);
  assert.deepEqual(r.statuses.returning, [0, 1, 1, 0, 0]);
  // B is active on the 2nd without the 1st and was created earlier; A is back on the 4th after a gap.
  assert.deepEqual(r.statuses.resurrecting, [0, 1, 0, 1, 0]);
  assert.deepEqual(r.statuses.dormant, [0, 0, -1, -2, -1]);
});

test("stickiness counts people by the number of active days", { skip: skipNoPg }, async () => {
  const r = await run<StickinessResult>(exact, { kind: "stickiness", date_range: { date_from: "2026-09-01", date_to: "2026-09-05" }, series: [{ event: "open" }] });
  assert.deepEqual(r.series[0].data, [1, 1, 1, 0, 0]);
});

test("day retention on exact people, first time and recurring", { skip: skipNoPg }, async () => {
  const q = { kind: "retention", date_range: { date_from: "2026-08-01", date_to: "2026-09-04" }, period: "day", intervals: 4, start: { event: "open" }, return: { event: "open" } };
  const first = await run<RetentionResult>(exact, q);
  assert.deepEqual(first.cohorts.map((c) => c.label), ["2026-09-01", "2026-09-02", "2026-09-03", "2026-09-04"]);
  assert.deepEqual(first.cohorts.map((c) => c.values), [[1, 1, 0, 1], [1, 1, 0], [1, 0], [0]]);
  const rec = await run<RetentionResult>(exact, { ...q, retention_type: "recurring" });
  assert.deepEqual(rec.cohorts.map((c) => c.values), [[1, 1, 0, 1], [2, 1, 1], [2, 0], [1]]);
});

test("paths: sessions split at 30 minutes, repeats collapse, start and end points trim", { skip: skipNoPg }, async () => {
  const q = { kind: "paths", date_range: { date_from: "2026-09-01", date_to: "2026-09-03" }, path_type: "pageview" };
  const edges = (r: PathsResult) => r.edges.map((e) => `${e.source_step}:${e.source}>${e.target}=${e.people}`).sort();
  const all = await run<PathsResult>(exact, q);
  assert.deepEqual(edges(all), ["1:/a>/b=1", "1:/a>/c=1", "2:/b>/c=1"]);
  const fromB = await run<PathsResult>(exact, { ...q, start_point: "/b" });
  assert.deepEqual(edges(fromB), ["1:/b>/c=1"]);
  const toC = await run<PathsResult>(exact, { ...q, end_point: "/c", depth: 2 });
  assert.deepEqual(edges(toC), ["1:/a>/c=1", "1:/b>/c=1"]);
  const between = await run<PathsResult>(exact, { ...q, start_point: "/a", end_point: "/c" });
  assert.deepEqual(edges(between), ["1:/a>/b=1", "1:/a>/c=1", "2:/b>/c=1"]);
  assert.equal(all.edges.find((e) => e.source === "/a" && e.target === "/b")?.avg_seconds, 60);
});

const DAYS = { day: "1 day", week: "1 week", month: "1 month" } as const;

async function handRetention(period: "day" | "week" | "month", n: number, to: string, recurring: boolean): Promise<Map<string, number[]>> {
  const rows = await f.hand<{ cohort: string; k: number; v: string }>(
    `with ev as (${RESOLVED} and e."timestamp" <= $2),
     firsts as (select pid, date_trunc($3, min(ts) at time zone 'UTC') as c from ev where event = '$pageview' group by pid),
     starts as (select distinct pid, date_trunc($3, ts at time zone 'UTC') as c from ev where event = '$pageview'),
     bounds as (select date_trunc($3, $2::timestamptz at time zone 'UTC') - ($4::int - 1) * $5::interval as lo),
     cohorts as (select * from ${recurring ? "starts" : "firsts"} where c >= (select lo from bounds))
     select to_char(c.c, 'YYYY-MM-DD') as cohort, k,
            count(*) filter (where k = 0 or exists (
              select 1 from ev r where r.pid = c.pid and r.event = 'signup' and date_trunc($3, r.ts at time zone 'UTC') = c.c + k * $5::interval)) as v
       from cohorts c cross join generate_series(0, $4::int - 1) as k
      where c.c + k * $5::interval <= date_trunc($3, $2::timestamptz at time zone 'UTC')
      group by 1, 2 order by 1, 2`,
    [f.projectId, to, period, n, DAYS[period]],
  );
  const out = new Map<string, number[]>();
  for (const r of rows) {
    const list = out.get(r.cohort) ?? [];
    list[r.k] = Number(r.v);
    out.set(r.cohort, list);
  }
  return out;
}

test("retention grids match hand SQL on the fixture (day, week, month; first time and recurring)", { skip: skipNoPg }, async () => {
  for (const [period, n, recurring] of [
    ["week", 8, false],
    ["week", 8, true],
    ["day", 14, false],
    ["month", 2, true],
  ] as const) {
    const r = await run<RetentionResult>(f.projectId, {
      kind: "retention",
      date_range: { date_from: "-60d", date_to: "2026-09-29" },
      period,
      intervals: n,
      retention_type: recurring ? "recurring" : "first_time",
      start: { event: "$pageview" },
      return: { event: "signup" },
    });
    const hand = await handRetention(period, n, "2026-09-29T23:59:59.999Z", recurring);
    const got = new Map(r.cohorts.filter((c) => c.size > 0).map((c) => [period === "month" ? `${c.label}-01` : c.label, c.values]));
    assert.equal(r.cohorts.length, n, `${period}: ${n} rows`);
    assert.deepEqual(got, hand, `${period} ${recurring ? "recurring" : "first time"}`);
    assert.ok([...hand.values()].some((v) => v.slice(1).some((x) => x > 0)), "the fixture has returns");
  }
});

test("stickiness matches hand SQL on the fixture", { skip: skipNoPg }, async () => {
  const r = await run<StickinessResult>(f.projectId, { kind: "stickiness", date_range: { date_from: "2026-09-01", date_to: "2026-09-29" }, series: [{ event: "$pageview" }, { event: "signup" }] });
  for (const [i, ev] of ["$pageview", "signup"].entries()) {
    const rows = await f.hand<{ n: number; people: string }>(
      `with days as (select pid, count(distinct date_trunc('day', ts)) as n from (${RESOLVED} and e.event = $2 and e."timestamp" >= '2026-09-01' and e."timestamp" < '2026-09-30') x group by pid)
       select n::int, count(*) as people from days group by n`,
      [f.projectId, ev],
    );
    const want = Array.from({ length: 29 }, () => 0);
    for (const x of rows) want[x.n - 1] = Number(x.people);
    assert.deepEqual(r.series[i].data, want, ev);
  }
});

test("lifecycle matches hand SQL on the fixture (day and week)", { skip: skipNoPg }, async () => {
  for (const [interval, from, to] of [
    ["day", "2026-09-01", "2026-09-20"],
    ["week", "2026-08-03", "2026-09-27"],
  ] as const) {
    const r = await run<LifecycleResult>(f.projectId, { kind: "lifecycle", interval, date_range: { date_from: from, date_to: to }, series: [{ event: "signup" }] });
    const rows = await f.hand<{ b: string; new: string; returning: string; resurrecting: string; dormant: string }>(
      `with act as (
         select distinct x.pid, date_trunc($2, x.ts at time zone 'UTC') as b
           from (${RESOLVED} and e.event = 'signup') x
          where x.ts >= date_trunc($2, $3::timestamptz at time zone 'UTC') - $5::interval
            and x.ts < date_trunc($2, $4::timestamptz at time zone 'UTC') + $5::interval
            and exists (select 1 from pa.persons p where p.id = x.pid)
       ),
       first_seen as (select pid, min(b) as b from act group by pid),
       created as (select id as pid, date_trunc($2, created_at at time zone 'UTC') as c from pa.persons where project_id = $1),
       axis as (select generate_series(date_trunc($2, $3::timestamptz at time zone 'UTC'), date_trunc($2, $4::timestamptz at time zone 'UTC'), $5::interval) as b)
       select to_char(a.b, 'YYYY-MM-DD') as b,
         (select count(*) from act x join first_seen fs on fs.pid = x.pid join created c on c.pid = x.pid where x.b = a.b and fs.b = a.b and c.c = a.b) as new,
         (select count(*) from act x where x.b = a.b and exists (select 1 from act y where y.pid = x.pid and y.b = a.b - $5::interval)) as returning,
         (select count(*) from act x join first_seen fs on fs.pid = x.pid join created c on c.pid = x.pid
           where x.b = a.b and not exists (select 1 from act y where y.pid = x.pid and y.b = a.b - $5::interval) and not (fs.b = a.b and c.c = a.b)) as resurrecting,
         -(select count(*) from act x where x.b = a.b - $5::interval and not exists (select 1 from act y where y.pid = x.pid and y.b = a.b)) as dormant
       from axis a order by a.b`,
      [f.projectId, interval, from, `${to}T23:59:59.999Z`, interval === "day" ? "1 day" : "1 week"],
    );
    assert.deepEqual(r.labels, rows.map((x) => x.b));
    for (const s of ["new", "returning", "resurrecting", "dormant"] as const) assert.deepEqual(r.statuses[s], rows.map((x) => Number(x[s])), `${interval} ${s}`);
    assert.ok(r.statuses.resurrecting.some((x) => x > 0) && r.statuses.dormant.some((x) => x < 0));
  }
});

test("paths match hand SQL (session arrays) on the fixture, with start point and depth", { skip: skipNoPg }, async () => {
  for (const [start, depth] of [
    [null, 3],
    ["https://shop.example/pricing", 4],
  ] as const) {
    const r = await run<PathsResult>(f.projectId, {
      kind: "paths",
      date_range: { date_from: "2026-09-01", date_to: "2026-09-29" },
      path_type: "pageview",
      start_point: start,
      depth,
      edge_limit: 500,
    });
    const rows = await f.hand<{ i: number; a: string; b: string; n: string }>(
      `with ev as (${RESOLVED} and e.event = '$pageview' and e."timestamp" >= '2026-09-01' and e."timestamp" < '2026-09-30'),
       gaps as (select *, case when ts - lag(ts) over (partition by pid order by ts, uuid) < interval '30 minutes' then 0 else 1 end as new_session from ev),
       sessions as (select *, sum(new_session) over (partition by pid order by ts, uuid) as s from gaps),
       arrays as (select pid, s, array_agg(properties ->> '$current_url' order by ts, uuid) as items from sessions group by pid, s),
       compacted as (
         select pid, s, array(select v from unnest(items) with ordinality as u(v, n) where n = 1 or v is distinct from items[n - 1] order by n) as items from arrays
       ),
       started as (
         select pid, s, case when $2::text is null then items else items[array_position(items, $2::text):] end as items
           from compacted where $2::text is null or array_position(items, $2::text) is not null
       ),
       limited as (select pid, s, items[1:$3::int] as items from started)
       select i, items[i] as a, items[i + 1] as b, count(*) as n
         from limited cross join generate_series(1, $3::int - 1) as i
        where items[i + 1] is not null group by 1, 2, 3`,
      [f.projectId, start, depth],
    );
    const got = r.edges.map((e) => `${e.source_step}|${e.source}|${e.target}|${e.people}`).sort();
    const want = rows.map((x) => `${x.i}|${x.a}|${x.b}|${x.n}`).sort();
    assert.ok(want.length > 5);
    assert.deepEqual(got, want, `start ${start} depth ${depth}`);
  }
});
