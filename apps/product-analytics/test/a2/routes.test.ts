// The insights console API end to end on the running service: org tokens
// and roles, isolation between apps, every endpoint (query with cache,
// funnel people, time to convert, saved insights, dashboards with refresh,
// actions, cohorts with CSV, SQL with CSV download, meta), the privacy hook
// and the scheduled tasks.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import pg from "pg";
import { insightsTasks } from "../../src/insights/jobs.ts";
import { signConsoleToken } from "../../src/tenancy/console-token.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { loadFixture } from "./fixtures.ts";
import { NOW } from "./setup.ts";

const SECRET = "q".repeat(48);
let pgc: ScratchPg;
let run: Running;
const a = makeSite({ name: "shop" });
const b = makeSite({ name: "other" });

before(async () => {
  if (skipNoPg) return;
  pgc = await migratedPg();
  const owner = new pg.Client({ connectionString: pgc.url });
  await owner.connect();
  await loadFixture(owner, { projectId: a.project_id, persons: 80, days: 30, end: NOW, seed: 5 });
  await loadFixture(owner, { projectId: b.project_id, persons: 20, days: 30, end: NOW, seed: 6 });
  await owner.end();
  run = await startService(pgc, [a, b], { consoleSecret: SECRET, now: () => NOW });
});

async function call(path: string, init: RequestInit & { role?: "owner" | "viewer"; site?: typeof a; json?: unknown } = {}) {
  const site = init.site ?? a;
  const tok = await signConsoleToken(SECRET, { org_id: site.org_id, app_ids: [site.project_id], role: init.role ?? "owner" }, NOW);
  const headers: Record<string, string> = { authorization: `Bearer ${tok}` };
  if (init.json !== undefined) headers["content-type"] = "application/json";
  const res = await fetch(`${run.base}/v1/projects/${site.project_id}/insights${path}`, { ...init, headers: { ...headers, ...(init.headers as Record<string, string>) }, body: init.json !== undefined ? JSON.stringify(init.json) : init.body });
  const text = await res.text();
  let data: any = text;
  try {
    data = JSON.parse(text);
  } catch {
    /* csv */
  }
  return { status: res.status, data, headers: res.headers };
}

const trends = { kind: "trends", date_range: { date_from: "-7d" }, series: [{ event: "$pageview" }] };
const funnel = { kind: "funnels", date_range: { date_from: "-30d" }, steps: [{ event: "$pageview" }, { event: "signup" }] };

test("tokens, roles and app isolation", { skip: skipNoPg }, async () => {
  assert.equal((await fetch(`${run.base}/v1/projects/${a.project_id}/insights/meta`)).status, 401);
  const tokA = await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id], role: "owner" }, NOW);
  assert.equal((await fetch(`${run.base}/v1/projects/${b.project_id}/insights/meta`, { headers: { authorization: `Bearer ${tokA}` } })).status, 403);
  const forged = await signConsoleToken(SECRET, { org_id: a.org_id, app_ids: [a.project_id, b.project_id], role: "owner" }, NOW);
  assert.equal((await fetch(`${run.base}/v1/projects/${b.project_id}/insights/sql`, { method: "POST", headers: { authorization: `Bearer ${forged}` }, body: JSON.stringify({ sql: "select 1" }) })).status, 403);
  const viewer = await call("/saved", { method: "POST", role: "viewer", json: { name: "x", query: trends } });
  assert.equal(viewer.status, 403);
  assert.match(viewer.data.error, /view this app but not change it/);
  assert.equal((await call("/query", { method: "POST", role: "viewer", json: { query: trends } })).status, 200);
  // A saved insight of app B is not found through app A.
  const other = await call("/saved", { method: "POST", site: b, json: { name: "B views", query: trends } });
  assert.equal(other.status, 201);
  assert.equal((await call(`/saved/${other.data.insight.id}`)).status, 404);
});

test("query: answers, cache, plain errors", { skip: skipNoPg }, async () => {
  const q = { ...trends, date_range: { date_from: "-6d" } };
  const first = await call("/query", { method: "POST", json: { query: q } });
  assert.equal(first.status, 200);
  assert.equal(first.data.result.kind, "trends");
  assert.equal(first.data.cached, false);
  const again = await call("/query", { method: "POST", json: { query: q } });
  assert.equal(again.data.cached, true);
  assert.equal(again.data.computed_at, first.data.computed_at);
  const forced = await call("/query", { method: "POST", json: { query: q, refresh: true } });
  assert.equal(forced.data.cached, false);
  const bad = await call("/query", { method: "POST", json: { query: { kind: "trends", series: [] } } });
  assert.equal(bad.status, 400);
  assert.match(bad.data.error, /series/);
  assert.equal((await call("/query", { method: "POST", body: "{not json", headers: { "content-type": "application/json" } })).status, 400);
  const meta = await call("/meta");
  assert.ok(meta.data.events.some((e: { event: string }) => e.event === "$pageview"));
  assert.ok(meta.data.person_properties.includes("plan"));
  assert.deepEqual(meta.data.group_types, [{ index: 0, name: "company" }]);
});

test("funnel people and time to convert", { skip: skipNoPg }, async () => {
  const dropped = await call("/funnel-people", { method: "POST", json: { query: funnel, step: 2, dropped: true } });
  assert.equal(dropped.status, 200);
  assert.ok(dropped.data.people.length > 0);
  assert.ok(dropped.data.people.every((p: { distinct_id: string }) => typeof p.distinct_id === "string"));
  const ttc = await call("/time-to-convert", { method: "POST", json: { query: funnel, bins: 5 } });
  assert.equal(ttc.data.bins.length, 5);
  assert.equal((await call("/time-to-convert", { method: "POST", json: { query: trends } })).status, 400);
});

test("saved insights and dashboards with refresh", { skip: skipNoPg }, async () => {
  const s = await call("/saved", { method: "POST", json: { name: "Views", query: trends } });
  const d = await call("/dashboards", { method: "POST", json: { name: "Main" } });
  assert.equal((await call(`/dashboards/${d.data.dashboard.id}/tiles`, { method: "POST", json: { insight_id: s.data.insight.id } })).status, 201);
  const view = await call(`/dashboards/${d.data.dashboard.id}`);
  assert.equal(view.data.dashboard.tiles.length, 1);
  const cached = await call(`/dashboards/${d.data.dashboard.id}`);
  assert.equal(cached.data.dashboard.tiles[0].answer.cached, true);
  const refreshed = await call(`/dashboards/${d.data.dashboard.id}?refresh=1`);
  assert.equal(refreshed.data.dashboard.tiles[0].answer.cached, false);
  assert.equal((await call("/dashboards")).data.dashboards[0].tiles, 1);
  assert.equal((await call(`/dashboards/${d.data.dashboard.id}/tiles/${s.data.insight.id}`, { method: "DELETE" })).status, 200);
  assert.equal((await call(`/saved/${s.data.insight.id}`, { method: "PUT", json: { name: "Views weekly", query: { ...trends, interval: "week" } } })).data.insight.name, "Views weekly");
  assert.equal((await call(`/dashboards/${d.data.dashboard.id}`, { method: "DELETE" })).status, 200);
});

test("actions and cohorts, including a CSV upload", { skip: skipNoPg }, async () => {
  const act = await call("/actions", { method: "POST", json: { name: "Buy click", steps: [{ event: "$autocapture", selector: "button.primary" }] } });
  assert.equal(act.status, 201);
  assert.equal((await call("/actions")).data.actions.length, 1);
  const bad = await call("/actions", { method: "POST", json: { name: "Empty", steps: [{}] } });
  assert.equal(bad.status, 400);
  const beh = await call("/cohorts", { method: "POST", json: { name: "Signed up", kind: "behavioural", definition: { criteria: [{ type: "behaviour", event: "signup", operator: "gte", count: 1, days: 30 }] } } });
  assert.equal(beh.status, 201);
  assert.ok(beh.data.cohort.people_count > 0, "computed on create");
  const st = await call("/cohorts", { method: "POST", json: { name: "List", kind: "static" } });
  const up = await call(`/cohorts/${st.data.cohort.id}/csv`, { method: "POST", body: "distinct_id\nuser_1\nuser_2\n", headers: { "content-type": "text/csv" } });
  assert.equal(up.status, 200);
  assert.equal(up.data.cohort.people_count, 2);
  const filtered = await call("/query", { method: "POST", json: { query: { ...trends, properties: [{ type: "cohort", cohort_id: st.data.cohort.id }] } } });
  assert.equal(filtered.status, 200);
  assert.equal((await call(`/cohorts/${beh.data.cohort.id}/recompute`, { method: "POST" })).status, 200);
  assert.equal((await call(`/cohorts/${st.data.cohort.id}/recompute`, { method: "POST" })).status, 400);
});

test("SQL answers as JSON and as a CSV download; refusals are plain", { skip: skipNoPg }, async () => {
  const r = await call("/sql", { method: "POST", json: { sql: "select event, count(*) as n from pa_view.events group by 1 order by 2 desc limit 3" } });
  assert.equal(r.status, 200);
  assert.deepEqual(r.data.columns, ["event", "n"]);
  assert.equal(r.data.rows.length, 3);
  const csv = await call("/sql.csv", { method: "POST", json: { sql: "select event from pa_view.events limit 2" } });
  assert.equal(csv.headers.get("content-type"), "text/csv; charset=utf-8");
  assert.match(csv.headers.get("content-disposition") ?? "", /attachment/);
  assert.match(String(csv.data), /^event\r\n/);
  const refused = await call("/sql", { method: "POST", json: { sql: "select * from pa.events" } });
  assert.equal(refused.status, 400);
  assert.match(refused.data.error, /pa_view/);
});

test("the privacy hook is registered and the scheduled tasks run", { skip: skipNoPg }, async () => {
  assert.ok(run.svc.deps.hooks.hooks.some((h) => h.name === "cohorts"));
  const tasks = insightsTasks(run.svc.deps);
  assert.deepEqual(tasks.map((t) => t.name), ["insights-rollups", "insights-cohorts", "insights-cache-prune"]);
  for (const t of tasks) await t.run(new Date());
  const rolled = await run.svc.deps.db.query<{ n: string }>("select count(*) as n from pa.rollup_state");
  assert.equal(Number(rolled[0].n), 2);
});
