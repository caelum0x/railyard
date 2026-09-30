// Actions (selector, tag, text, link and address matching over the
// elements chain, ported from upstream) and cohorts (behavioural and static
// from CSV). A cohort filters every insight type; its members survive a
// merge; a privacy deletion removes a person from every cohort.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createAction } from "../../src/insights/actions.ts";
import { cohortsGdprHook, createCohort, importCohortCsv, listCohorts, recomputeCohort } from "../../src/insights/cohorts.ts";
import { parseSelector, selectorRegex, tagNameRegex } from "../../src/insights/elements.ts";
import { runInsight, type FunnelsResult, type TrendsResult } from "../../src/insights/runner.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { BUY_CHAIN, PRICING_CHAIN } from "./fixtures.ts";
import { fixtureDb, NOW, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb();
});

test("selectors parse and compile like upstream", () => {
  const parts = parseSelector("div > form button#buy.btn.primary");
  assert.equal(parts.length, 3);
  assert.deepEqual(parts[0], { directDescendant: false, tagName: "button", classes: ["btn", "primary"], chAttributes: { attr_id: "buy" } });
  // Classes are split before the id, as upstream does: "#" after a class stays in the class.
  assert.deepEqual(parseSelector("button.primary#buy")[0].classes, ["primary#buy"]);
  assert.equal(parts[2].directDescendant, true);
  const re = (s: string) => new RegExp(selectorRegex(s).replace(/\(\?:/g, "("));
  assert.ok(re("button.primary").test(BUY_CHAIN));
  assert.ok(re("form button").test(BUY_CHAIN));
  assert.ok(re("#buy").test(BUY_CHAIN));
  assert.ok(re("[data-x='1']").test('div:attr__data-x="1"nth-child="1"'));
  assert.ok(!re("button.secondary").test(BUY_CHAIN));
  assert.ok(re("a.nav-link").test(PRICING_CHAIN));
  assert.equal(tagNameRegex("a.b"), "(^|;)a\\.b(\\.|$|;|:)");
  assert.equal(selectorRegex(""), "");
});

async function count(where: string, params: unknown[] = []): Promise<number> {
  const [{ n }] = await f.hand<{ n: string }>(`select count(*) as n from pa.events e where e.project_id = $1 and e."timestamp" >= '2026-08-31' and ${where}`, [f.projectId, ...params]);
  return Number(n);
}

const trendsTotal = async (series: Record<string, unknown>, extra: Record<string, unknown> = {}) =>
  ((await runInsight(f.db, f.projectId, parseQuery({ kind: "trends", date_range: { date_from: "-30d" }, series: [series], ...extra }), NOW)) as TrendsResult).series[0].total;

test("actions match autocapture by selector, tag, text and link, and page views by address", { skip: skipNoPg }, async () => {
  const buy = await createAction(f.db, f.projectId, { name: "Clicked buy", steps: [{ event: "$autocapture", selector: "form button.primary" }] });
  assert.equal(await trendsTotal({ kind: "action", action_id: buy.id }), await count(`e.event = '$autocapture' and e.elements_chain = $2`, [BUY_CHAIN]));
  const text = await createAction(f.db, f.projectId, { name: "Buy text", steps: [{ event: "$autocapture", text: "Buy now" }] });
  assert.equal(await trendsTotal({ kind: "action", action_id: text.id }), await count(`e.event = '$autocapture' and e.elements_chain = $2`, [BUY_CHAIN]));
  const link = await createAction(f.db, f.projectId, { name: "Pricing link", steps: [{ event: "$autocapture", href: "pric", href_matching: "contains", tag_name: "a" }] });
  assert.equal(await trendsTotal({ kind: "action", action_id: link.id }), await count(`e.event = '$autocapture' and e.elements_chain = $2`, [PRICING_CHAIN]));
  // Several steps are OR-ed: the buy click or a signup page view.
  const either = await createAction(f.db, f.projectId, {
    name: "Buy or signup page",
    steps: [
      { event: "$autocapture", selector: "#buy" },
      { event: "$pageview", url: "/signup", url_matching: "contains" },
    ],
  });
  assert.equal(
    await trendsTotal({ kind: "action", action_id: either.id }),
    await count(`((e.event = '$autocapture' and e.elements_chain = $2) or (e.event = '$pageview' and e.properties ->> '$current_url' like '%/signup%'))`, [BUY_CHAIN]),
  );
  // An action is a funnel step too.
  const funnel = (await runInsight(f.db, f.projectId, parseQuery({ kind: "funnels", date_range: { date_from: "-30d" }, steps: [{ event: "$pageview" }, { kind: "action", action_id: buy.id }] }), NOW)) as FunnelsResult;
  assert.ok(funnel.groups[0].steps[1].people > 0);
  await assert.rejects(createAction(f.db, f.projectId, { name: "Clicked buy", steps: [{ event: "x" }] }), /already/);
});

test("a behavioural cohort (did signup at least 2 times in 30 days, plan pro) matches hand SQL", { skip: skipNoPg }, async () => {
  const c = await createCohort(f.db, f.projectId, {
    name: "Repeat pro signups",
    kind: "behavioural",
    definition: {
      match: "all",
      criteria: [
        { type: "behaviour", event: "signup", operator: "gte", count: 2, days: 30 },
        { type: "person", key: "plan", operator: "exact", value: "pro" },
      ],
    },
  });
  const done = await recomputeCohort(f.db, f.projectId, c.id, NOW);
  const [{ n }] = await f.hand<{ n: string }>(
    `select count(*) as n from pa.persons p
      where p.project_id = $1 and p.properties ->> 'plan' = 'pro'
        and (select count(*) from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
              where e.project_id = $1 and coalesce(o.new_person_id, e.person_id) = p.id and e.event = 'signup' and e."timestamp" >= $2::timestamptz - interval '30 days') >= 2`,
    [f.projectId, NOW.toISOString()],
  );
  assert.ok(Number(n) > 0);
  assert.equal(done.people_count, Number(n));
  const [{ m }] = await f.hand<{ m: string }>("select count(*) as m from pa.cohort_people where project_id = $1 and cohort_id = $2", [f.projectId, c.id]);
  assert.equal(Number(m), Number(n));
  // "Did not purchase in 30 days": at most 0 times, any match.
  const lapsed = await createCohort(f.db, f.projectId, { name: "No purchase", kind: "behavioural", definition: { match: "any", criteria: [{ type: "behaviour", event: "purchase", operator: "lte", count: 0, days: 30 }] } });
  const l = await recomputeCohort(f.db, f.projectId, lapsed.id, NOW);
  const [{ k }] = await f.hand<{ k: string }>(
    `select count(*) as k from pa.persons p where p.project_id = $1 and not exists (
       select 1 from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
        where e.project_id = $1 and coalesce(o.new_person_id, e.person_id) = p.id and e.event = 'purchase' and e."timestamp" >= $2::timestamptz - interval '30 days')`,
    [f.projectId, NOW.toISOString()],
  );
  assert.equal(l.people_count, Number(k));
});

test("a static cohort imports from CSV by distinct id or email, including merged ids", { skip: skipNoPg }, async () => {
  const c = await createCohort(f.db, f.projectId, { name: "Beta list", kind: "static", definition: {} });
  const csv = 'distinct_id,note\nuser_1,"first, one"\nanon_2\nuser_2\nnobody_here\n';
  const r = await importCohortCsv(f.db, f.projectId, c.id, csv, NOW);
  const [{ n }] = await f.hand<{ n: string }>(
    "select count(distinct person_id) as n from pa.person_distinct_ids where project_id = $1 and distinct_id in ('user_1', 'anon_2', 'user_2')",
    [f.projectId],
  );
  assert.equal(r.people_count, Number(n));
  const known = await f.hand("select 1 from pa.person_distinct_ids where project_id = $1 and distinct_id in ('user_1', 'anon_2', 'user_2', 'nobody_here')", [f.projectId]);
  assert.equal(r.unmatched, 4 - known.length);
  const byEmail = await createCohort(f.db, f.projectId, { name: "Email list", kind: "static", definition: {} });
  const e = await importCohortCsv(f.db, f.projectId, byEmail.id, "email\nuser_3@example.com\nUSER_4@example.com\n", NOW);
  assert.equal(e.people_count, 2);
  await assert.rejects(importCohortCsv(f.db, f.projectId, byEmail.id, "name\nx\n", NOW), /distinct_id or email/);
  await assert.rejects(recomputeCohort(f.db, f.projectId, byEmail.id, NOW), /from a file/);
  const listed = await listCohorts(f.db, f.projectId);
  assert.ok(listed.some((x) => x.name === "Email list" && x.people_count === 2));
});

test("a cohort filters every insight type exactly like the same people's property filter", { skip: skipNoPg }, async () => {
  const ids = (await f.hand<{ d: string }>(
    "select min(d.distinct_id) as d from pa.persons p join pa.person_distinct_ids d on d.person_id = p.id where p.project_id = $1 and p.properties ->> 'plan' = 'team' group by p.id",
    [f.projectId],
  )).map((r) => r.d);
  const c = await createCohort(f.db, f.projectId, { name: "Team plan", kind: "static", definition: {} });
  await importCohortCsv(f.db, f.projectId, c.id, `distinct_id\n${ids.join("\n")}\n`, NOW);
  const byCohort = [{ type: "cohort", cohort_id: c.id }];
  const byProp = [{ type: "person", key: "plan", operator: "exact", value: "team" }];
  const range = { date_from: "2026-09-01", date_to: "2026-09-29" };
  const queries: Record<string, unknown>[] = [
    { kind: "trends", series: [{ event: "$pageview", math: "dau" }] },
    { kind: "funnels", steps: [{ event: "$pageview" }, { event: "signup" }, { event: "purchase" }] },
    { kind: "retention", period: "week", intervals: 4, start: { event: "$pageview" }, return: { event: "$pageview" } },
    { kind: "paths", path_type: "pageview", depth: 3 },
    { kind: "stickiness", series: [{ event: "$pageview" }] },
    { kind: "lifecycle", series: [{ event: "$pageview" }] },
  ];
  for (const q of queries) {
    const a = await runInsight(f.db, f.projectId, parseQuery({ ...q, date_range: range, properties: byCohort }), NOW);
    const b = await runInsight(f.db, f.projectId, parseQuery({ ...q, date_range: range, properties: byProp }), NOW);
    const all = await runInsight(f.db, f.projectId, parseQuery({ ...q, date_range: range }), NOW);
    assert.deepEqual(a, b, String(q.kind));
    assert.notDeepEqual(a, all, `${q.kind}: the cohort narrows the answer`);
  }
  // And against hand SQL for trends.
  const t = (await runInsight(f.db, f.projectId, parseQuery({ kind: "trends", date_range: range, series: [{ event: "$pageview" }], properties: byCohort }), NOW)) as TrendsResult;
  const [{ n }] = await f.hand<{ n: string }>(
    `select count(*) as n from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
      where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= '2026-09-01' and e."timestamp" < '2026-09-30'
        and coalesce(o.new_person_id, e.person_id) in (select person_id from pa.cohort_people where cohort_id = $2)`,
    [f.projectId, c.id],
  );
  assert.equal(t.series[0].total, Number(n));
  // A negated cohort is everyone else.
  const not = (await runInsight(f.db, f.projectId, parseQuery({ kind: "trends", date_range: range, series: [{ event: "$pageview" }], properties: [{ type: "cohort", cohort_id: c.id, negate: true }] }), NOW)) as TrendsResult;
  const every = (await runInsight(f.db, f.projectId, parseQuery({ kind: "trends", date_range: range, series: [{ event: "$pageview" }] }), NOW)) as TrendsResult;
  assert.equal((not.series[0].total ?? 0) + (t.series[0].total ?? 0), every.series[0].total);
});

test("a privacy deletion removes the person from every cohort and drops cached answers", { skip: skipNoPg }, async () => {
  const [{ id }] = await f.hand<{ id: string }>("select person_id as id from pa.cohort_people where project_id = $1 limit 1", [f.projectId]);
  await f.hand("insert into pa.insight_cache (project_id, cache_key, result, data_version, computed_at) values ($1, repeat('a', 64), '{}', '1', now())", [f.projectId]);
  const removed = await cohortsGdprHook.deletePerson({ db: f.db, projectId: f.projectId, personIds: [id], distinctIds: [], now: NOW });
  assert.ok(removed >= 1);
  assert.equal((await f.hand("select 1 from pa.cohort_people where project_id = $1 and person_id = $2", [f.projectId, id])).length, 0);
  assert.equal((await f.hand("select 1 from pa.insight_cache where project_id = $1", [f.projectId])).length, 0);
  const [{ r }] = await f.hand<{ r: string | null }>("select reconcile_after as r from pa.rollup_state where project_id = $1", [f.projectId]);
  assert.ok(r, "rollups are rebuilt for the project");
  const exported = await cohortsGdprHook.exportPerson({ db: f.db, projectId: f.projectId, personIds: [id], distinctIds: [], now: NOW });
  assert.deepEqual(exported, []);
});
