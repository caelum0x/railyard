// Funnels: ordered, strict and unordered match hand-written lateral SQL on
// the shared fixture (window, breakdown, merged people); exact cases
// re-expressed from the upstream funnel tests pin the edge rules (window
// from the first step, several tries, one event never fills two steps,
// strict next event, any order); time to convert and drop-off people.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { runFunnelPeople, runInsight, runTimeToConvert, type FunnelsResult } from "../../src/insights/runner.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import type { FunnelsQuery } from "../../src/insights/types.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { insertEvents } from "./fixtures.ts";
import { fixtureDb, NOW, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
const exact = randomUUID();
const P = Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`p${i + 1}`, randomUUID()])) as Record<string, string>;

before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb();
  const d = (day: number, h = 0, m = 0) => new Date(Date.UTC(2026, 8, 1 + day, h, m)).toISOString();
  await insertEvents(f.owner, exact, [
    // p1: the first try runs out of window; the second try (day 8) converts.
    { person: P.p1, event: "signup", at: d(0), properties: { source: "ads" } },
    { person: P.p1, event: "signup", at: d(8), properties: { source: "search" } },
    { person: P.p1, event: "view", at: d(10) },
    { person: P.p1, event: "buy", at: d(20) },
    // p2: another event sits between the steps (not strict).
    { person: P.p2, event: "signup", at: d(1), properties: { source: "ads" } },
    { person: P.p2, event: "other", at: d(1, 1) },
    { person: P.p2, event: "view", at: d(1, 2) },
    { person: P.p2, event: "buy", at: d(1, 3) },
    // p3: all three, in reverse order.
    { person: P.p3, event: "buy", at: d(2) },
    { person: P.p3, event: "view", at: d(2, 1) },
    { person: P.p3, event: "signup", at: d(2, 2), properties: { source: "friend" } },
    // p4: one view only; p5: two views.
    { person: P.p4, event: "view", at: d(3) },
    { person: P.p5, event: "view", at: d(3) },
    { person: P.p5, event: "view", at: d(3, 5) },
    // p6: strict and quick: 1 hour, then 2 more hours.
    { person: P.p6, event: "signup", at: d(4), properties: { source: "ads" } },
    { person: P.p6, event: "view", at: d(4, 1) },
    { person: P.p6, event: "buy", at: d(4, 3) },
    // p7: signs up and leaves.
    { person: P.p7, event: "signup", at: d(5), properties: { source: "search" } },
  ]);
});

const steps = (...names: string[]) => names.map((event) => ({ event }));
const funnel = async (projectId: string, q: Record<string, unknown>) =>
  (await runInsight(f.db, projectId, parseQuery({ kind: "funnels", date_range: { date_from: "2026-09-01", date_to: "2026-09-29" }, ...q }), NOW)) as FunnelsResult;
const people = (r: FunnelsResult, g = 0) => r.groups[g].steps.map((s) => s.people);

test("ordered: the window runs from the first step, and a later try can convert", { skip: skipNoPg }, async () => {
  const r = await funnel(exact, { steps: steps("signup", "view", "buy"), window: { value: 14, unit: "day" } });
  // p1 (second try), p2, p6 convert; p3 has signup last; p7 stops at 1.
  assert.deepEqual(people(r), [5, 3, 3]);
  const r7 = await funnel(exact, { steps: steps("signup", "view", "buy"), window: { value: 7, unit: "day" } });
  // With 7 days p1's second try reaches view (day 10) but not buy (day 20).
  assert.deepEqual(people(r7), [5, 3, 2]);
  assert.deepEqual(r.groups[0].steps.map((s) => Math.round(s.from_first * 100)), [100, 60, 60]);
  assert.deepEqual(r.groups[0].steps.map((s) => s.dropped), [0, 2, 0]);
});

test("one event never fills two steps", { skip: skipNoPg }, async () => {
  const r = await funnel(exact, { steps: steps("view", "view") });
  // p1, p2, p3, p4, p5, p6 viewed; only p5 viewed twice.
  assert.deepEqual(people(r), [6, 1]);
});

test("strict: each step must be the very next event", { skip: skipNoPg }, async () => {
  const r = await funnel(exact, { steps: steps("signup", "view", "buy"), order: "strict" });
  // p1: signup(8) -> view(10) -> buy(20) is consecutive and within 14 days; p2 has "other" between; p6 converts.
  assert.deepEqual(people(r), [5, 2, 2]);
});

test("unordered: any order within the window of the first", { skip: skipNoPg }, async () => {
  const r = await funnel(exact, { steps: steps("signup", "view", "buy"), order: "unordered" });
  // p3 now converts; p4 and p5 count at step 1 (one step done); p7 at step 1.
  assert.deepEqual(people(r), [7, 4, 4]);
});

test("breakdown takes the entry event's value; time to convert per step", { skip: skipNoPg }, async () => {
  const r = await funnel(exact, { steps: steps("signup", "view", "buy"), breakdown: { type: "event", key: "source" } });
  const byValue = new Map(r.groups.map((g) => [g.breakdown, people({ groups: [g] } as FunnelsResult)]));
  // p1 enters with ads (day 0) and search (day 8); only the search try converts.
  // ads: p1's first try reaches view only; p2 and p6 convert.
  assert.deepEqual(byValue.get("ads"), [3, 3, 2]);
  assert.deepEqual(byValue.get("search"), [2, 1, 1]);
  assert.deepEqual(byValue.get("friend"), [1, 0, 0]);
  const ttc = await funnel(exact, { steps: steps("signup", "view", "buy"), properties: [{ type: "event", key: "source", operator: "exact", value: "ads" }] });
  assert.equal(ttc.groups[0].steps[0].avg_seconds, null);
  // The filter applies to every step's event, so only people whose view and buy carry source=ads remain: none.
  assert.deepEqual(people(ttc), [3, 0, 0]);
  const quick = await funnel(exact, { steps: steps("signup", "view", "buy"), window: { value: 4, unit: "hour" } });
  assert.deepEqual(people(quick), [5, 2, 2]);
  assert.deepEqual(
    quick.groups[0].steps.map((s) => s.avg_seconds),
    [null, (3600 + 7200) / 2, (3600 + 3600 * 2) / 2],
  );
});

test("drop-off and converted people lists, and the time-to-convert histogram", { skip: skipNoPg }, async () => {
  const q = parseQuery({ kind: "funnels", date_range: { date_from: "2026-09-01", date_to: "2026-09-29" }, steps: steps("signup", "view", "buy") }) as FunnelsQuery;
  const dropped = await runFunnelPeople(f.db, exact, q, NOW, 2, true);
  assert.deepEqual(new Set(dropped.map((p) => p.id)), new Set([P.p3, P.p7]));
  const converted = await runFunnelPeople(f.db, exact, q, NOW, 3, false);
  assert.deepEqual(new Set(converted.map((p) => p.id)), new Set([P.p1, P.p2, P.p6]));
  await assert.rejects(runFunnelPeople(f.db, exact, q, NOW, 1, true), /first step/);
  const h = await runTimeToConvert(f.db, exact, q, NOW, 5);
  assert.equal(h.total, 3);
  assert.equal(h.bins.reduce((a, b) => a + b.people, 0), 3);
  assert.equal(h.bins.length, 5);
});

// Reference answers for the generated fixture, written as plain lateral joins.
const RANGE_FROM = "2026-09-01T00:00:00Z";
const RANGE_TO = "2026-09-29T23:59:59.999Z";
const EV = `select coalesce(o.new_person_id, e.person_id) as pid, e.uuid, e."timestamp" as ts, e.event, e.properties
              from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
             where e.project_id = $1 and e."timestamp" >= $2 and e."timestamp" <= $3`;

/** The fixture's events in range as an indexed temp table `ev` (plain lateral lookups stay fast). */
async function evTable(events: string[] | null): Promise<void> {
  await f.owner.query("drop table if exists ev");
  await f.owner.query("create temp table ev (pid uuid, uuid uuid, ts timestamptz, event text, properties jsonb)");
  await f.owner.query(`insert into ev ${EV}${events ? ` and e.event = any($4)` : ""}`, events ? [f.projectId, RANGE_FROM, RANGE_TO, events] : [f.projectId, RANGE_FROM, RANGE_TO]);
  await f.owner.query("create index on ev (pid, ts, uuid)");
  await f.owner.query("analyze ev");
}

async function handOrdered(names: string[], windowDays: number, strict: boolean): Promise<number[]> {
  const [a, b, c] = names;
  await evTable(strict ? null : names);
  const next = (alias: string, prev: string, name: string, cond: string) =>
    strict
      ? `left join lateral (select x.ts, x.uuid, x.event from ev x where ${cond} and x.pid = s.pid and (x.ts, x.uuid) > (${prev}.ts, ${prev}.uuid) order by x.ts, x.uuid limit 1) ${alias}_n on true
         left join lateral (select ${alias}_n.ts, ${alias}_n.uuid where ${alias}_n.event = '${name}' and ${alias}_n.ts <= s.ts + $1::int * interval '1 day') ${alias} on true`
      : `left join lateral (select x.ts, x.uuid from ev x where ${cond} and x.pid = s.pid and x.event = '${name}' and (x.ts, x.uuid) > (${prev}.ts, ${prev}.uuid)
                              and x.ts <= s.ts + $1::int * interval '1 day' order by x.ts, x.uuid limit 1) ${alias} on true`;
  const rows = await f.hand<{ r1: string; r2: string; r3: string }>(
    `with s as (select * from ev where event = '${a}'),
     tries as (
       select s.pid,
              case when c.ts is not null then 3 when b.ts is not null then 2 else 1 end as reached
         from s
         ${next("b", "s", b, "true")}
         ${next("c", "b", c, "b.ts is not null")}
     ),
     per as (select pid, max(reached) as r from tries group by pid)
     select count(*) filter (where r >= 1) as r1, count(*) filter (where r >= 2) as r2, count(*) filter (where r >= 3) as r3 from per`,
    [windowDays],
  );
  return [Number(rows[0].r1), Number(rows[0].r2), Number(rows[0].r3)];
}

test("ordered and strict funnels match hand SQL on the generated fixture", { skip: skipNoPg }, async () => {
  const names = ["$pageview", "signup", "purchase"];
  for (const [order, days] of [
    ["ordered", 14],
    ["ordered", 1],
    ["strict", 14],
  ] as const) {
    const r = await funnel(f.projectId, { steps: steps(...names), order, window: { value: days, unit: "day" } });
    const hand = await handOrdered(names, days, order === "strict");
    assert.ok(hand[2] > 0, `${order} ${days}: the fixture has converters`);
    assert.deepEqual(people(r), hand, `${order} ${days}`);
  }
});

test("unordered funnels match hand SQL on the generated fixture", { skip: skipNoPg }, async () => {
  const names = ["activate", "signup", "purchase"];
  const r = await funnel(f.projectId, { steps: steps(...names), order: "unordered", window: { value: 3, unit: "day" } });
  await evTable(names);
  const rows = await f.hand<{ r1: string; r2: string; r3: string }>(
    `with tries as (
       select a.pid,
              (exists (select 1 from ev x where x.pid = a.pid and x.event = 'activate' and (x.ts, x.uuid) >= (a.ts, a.uuid) and x.ts <= a.ts + interval '3 days'))::int
            + (exists (select 1 from ev x where x.pid = a.pid and x.event = 'signup' and (x.ts, x.uuid) >= (a.ts, a.uuid) and x.ts <= a.ts + interval '3 days'))::int
            + (exists (select 1 from ev x where x.pid = a.pid and x.event = 'purchase' and (x.ts, x.uuid) >= (a.ts, a.uuid) and x.ts <= a.ts + interval '3 days'))::int as reached
         from ev a
     ),
     per as (select pid, max(reached) as r from tries group by pid)
     select count(*) filter (where r >= 1) as r1, count(*) filter (where r >= 2) as r2, count(*) filter (where r >= 3) as r3 from per`,
  );
  assert.deepEqual(people(r), [Number(rows[0].r1), Number(rows[0].r2), Number(rows[0].r3)]);
  assert.ok(Number(rows[0].r3) > 0);
});

test("a person-property breakdown on the fixture matches hand SQL", { skip: skipNoPg }, async () => {
  const r = await funnel(f.projectId, { steps: steps("$pageview", "signup"), breakdown: { type: "person", key: "plan" } });
  await evTable(["$pageview", "signup"]);
  const rows = await f.hand<{ plan: string; r1: string; r2: string }>(
    `with tries as (
       select s.pid, s.plan, case when exists (
                select 1 from ev x where x.pid = s.pid and x.event = 'signup' and (x.ts, x.uuid) > (s.ts, s.uuid) and x.ts <= s.ts + interval '14 days') then 2 else 1 end as reached
         from (select ev.*, p.properties ->> 'plan' as plan from ev join pa.persons p on p.id = ev.pid where ev.event = '$pageview') s
     ),
     per as (select pid, plan, max(reached) as r from tries group by pid, plan)
     select plan, count(*) as r1, count(*) filter (where r >= 2) as r2 from per group by plan`,
  );
  const got = new Map(r.groups.map((g) => [g.breakdown, people({ groups: [g] } as FunnelsResult)]));
  assert.deepEqual(got, new Map(rows.map((x) => [x.plan, [Number(x.r1), Number(x.r2)]])));
});
