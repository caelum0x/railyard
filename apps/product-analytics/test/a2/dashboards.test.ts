// Saved insights, dashboards and the result cache: tiles hold saved
// insights; a dashboard refresh uses the cache; new data makes a cached
// answer stale, and a stale answer carries its computed-at time until a
// refresh recomputes it.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { cacheKey, computeCached, dataVersion } from "../../src/insights/cache.ts";
import { addTile, createDashboard, createSavedInsight, deleteSavedInsight, getDashboard, listDashboards, listSavedInsights, removeTile, updateSavedInsight } from "../../src/insights/store.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { skipNoPg } from "../a1/helpers.ts";
import { fixtureDb, NOW, type FixtureDb } from "./setup.ts";

let f: FixtureDb;
before(async () => {
  if (skipNoPg) return;
  f = await fixtureDb(120, 30);
});

const trend = { kind: "trends", date_range: { date_from: "-7d" }, series: [{ event: "$pageview" }] };
const funnel = { kind: "funnels", date_range: { date_from: "-7d" }, steps: [{ event: "$pageview" }, { event: "signup" }] };

test("the cache key ignores key order but not values; relative ranges roll daily", () => {
  const a = cacheKey(parseQuery(trend), NOW);
  const b = cacheKey(parseQuery({ series: [{ event: "$pageview" }], date_range: { date_from: "-7d" }, kind: "trends" }), NOW);
  assert.equal(a, b);
  assert.equal(a.length, 64);
  assert.notEqual(a, cacheKey(parseQuery({ ...trend, series: [{ event: "signup" }] }), NOW));
  assert.notEqual(a, cacheKey(parseQuery(trend), new Date(NOW.getTime() + 86_400_000)));
  assert.equal(a, cacheKey(parseQuery(trend), new Date(NOW.getTime() + 3600_000)));
});

test("saved insights: create, list, rename, change and delete", { skip: skipNoPg }, async () => {
  const s = await createSavedInsight(f.db, f.projectId, { name: "Views", query: trend });
  assert.equal(s.kind, "trends");
  const u = await updateSavedInsight(f.db, f.projectId, s.id, { name: "Page views", query: { ...trend, interval: "week" } });
  assert.equal(u?.name, "Page views");
  assert.equal((u?.query as { interval: string }).interval, "week");
  assert.ok((await listSavedInsights(f.db, f.projectId)).some((x) => x.id === s.id));
  assert.equal((await listSavedInsights(f.db, f.otherProjectId)).length, 0);
  await assert.rejects(createSavedInsight(f.db, f.projectId, { name: "", query: trend }), /name/);
  await assert.rejects(createSavedInsight(f.db, f.projectId, { name: "Bad", query: { kind: "nope" } }), /insight type/);
  assert.equal(await deleteSavedInsight(f.db, f.projectId, s.id), true);
  assert.equal(await deleteSavedInsight(f.db, f.projectId, s.id), false);
});

test("a dashboard computes its tiles once, then serves them from the cache", { skip: skipNoPg }, async () => {
  const d = await createDashboard(f.db, f.projectId, { name: "Weekly" });
  const t = await createSavedInsight(f.db, f.projectId, { name: "Views", query: trend });
  const fu = await createSavedInsight(f.db, f.projectId, { name: "Signup funnel", query: funnel });
  await addTile(f.db, f.projectId, d.id, t.id);
  await addTile(f.db, f.projectId, d.id, fu.id);
  const first = await getDashboard(f.db, f.projectId, d.id, NOW, { refresh: false });
  assert.ok(first);
  assert.equal(first.tiles.length, 2);
  assert.deepEqual(first.tiles.map((x) => x.insight.name), ["Views", "Signup funnel"]);
  assert.ok(first.tiles.every((x) => x.answer && !x.answer.cached && !x.answer.stale));
  const second = await getDashboard(f.db, f.projectId, d.id, NOW, { refresh: false });
  assert.ok(second?.tiles.every((x) => x.answer?.cached && !x.answer.stale));
  assert.equal(second?.tiles[0].answer?.computed_at, first.tiles[0].answer?.computed_at);
  assert.deepEqual(second?.tiles[0].answer?.result, first.tiles[0].answer?.result);
  assert.ok((await listDashboards(f.db, f.projectId)).some((x) => x.id === d.id && x.tiles === 2));
});

test("new data makes a cached answer stale with its computed-at time; refresh recomputes", { skip: skipNoPg }, async () => {
  const q = parseQuery(trend);
  const v1 = await dataVersion(f.db, f.projectId);
  const a = await computeCached(f.db, f.projectId, q, NOW, "prefer_cache");
  // New events arrive (the capture path adds to the month's stored count).
  await f.hand("insert into pa.usage_monthly (project_id, org_id, month, stored) values ($1, gen_random_uuid(), '2026-09-01', 5) on conflict (project_id, month) do update set stored = pa.usage_monthly.stored + 5", [f.projectId]);
  assert.notEqual(await dataVersion(f.db, f.projectId), v1);
  const stale = await computeCached(f.db, f.projectId, q, NOW, "prefer_cache");
  assert.equal(stale.stale, true);
  assert.equal(stale.cached, true);
  assert.equal(stale.computed_at, a.computed_at);
  const fresh = await computeCached(f.db, f.projectId, q, NOW, "fresh");
  assert.equal(fresh.stale, false);
  assert.equal(fresh.cached, false);
  assert.ok(Date.parse(fresh.computed_at) >= Date.parse(a.computed_at));
  const again = await computeCached(f.db, f.projectId, q, NOW, "prefer_cache");
  assert.equal(again.cached, true);
  assert.equal(again.stale, false);
  const forced = await computeCached(f.db, f.projectId, q, NOW, "force");
  assert.equal(forced.cached, false);
});

test("tiles go away with their insight; a dashboard of another project is not found", { skip: skipNoPg }, async () => {
  const d = await createDashboard(f.db, f.projectId, { name: "Short-lived" });
  const t = await createSavedInsight(f.db, f.projectId, { name: "Temp", query: trend });
  await addTile(f.db, f.projectId, d.id, t.id);
  await assert.rejects(addTile(f.db, f.projectId, d.id, t.id), /already/);
  assert.equal(await removeTile(f.db, f.projectId, d.id, t.id), true);
  await addTile(f.db, f.projectId, d.id, t.id);
  await deleteSavedInsight(f.db, f.projectId, t.id);
  assert.equal((await getDashboard(f.db, f.projectId, d.id, NOW, { refresh: false }))?.tiles.length, 0);
  assert.equal(await getDashboard(f.db, f.otherProjectId, d.id, NOW, { refresh: false }), null);
  await assert.rejects(addTile(f.db, f.otherProjectId, d.id, t.id), /dashboard|insight/);
});
