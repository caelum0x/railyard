// The insights console API, under /v1/projects/:id/insights/*. Every call
// carries the console's 5-minute org token; the project must be one of its
// app ids and belong to its org. Viewers read; other roles also write.
// Mounted by src/routes.ts (A1's mount line).

import { Hono, type Context } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { isUuid } from "../db.ts";
import { verifyConsoleToken, type ConsoleClaims } from "../tenancy/console-token.ts";
import type { Site } from "../tenancy/site-directory.ts";
import { createAction, deleteAction, listActions, updateAction } from "./actions.ts";
import { computeCached, type CacheMode } from "./cache.ts";
import { cohortsGdprHook, createCohort, deleteCohort, getCohort, importCohortCsv, listCohorts, MAX_CSV_BYTES, recomputeCohort } from "./cohorts.ts";
import { startInsightsJobs } from "./jobs.ts";
import { projectMeta } from "./meta.ts";
import { runFunnelPeople, runTimeToConvert } from "./runner.ts";
import { SqlSandbox, SqlUnavailable, toCsv } from "./sandbox.ts";
import { SqlRejected } from "./sandbox-parser.ts";
import { addTile, createDashboard, createSavedInsight, deleteDashboard, deleteSavedInsight, getDashboard, getSavedInsight, listDashboards, listSavedInsights, removeTile, updateSavedInsight } from "./store.ts";
import { QueryError, type FunnelsQuery } from "./types.ts";
import { parseQuery } from "./validate.ts";

type Env = { Variables: { claims: ConsoleClaims; site: Site } };
type C = Context<Env>;

export const MAX_JSON_BYTES = 256 * 1024;

class Forbidden extends Error {
  status = 403;
}

class TooMany extends Error {
  status = 429;
}

async function body(c: C): Promise<Record<string, unknown>> {
  const text = await c.req.text();
  if (text.length > MAX_JSON_BYTES) throw new QueryError("The request is too large.");
  if (!text) return {};
  try {
    const v = JSON.parse(text);
    if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error("not an object");
    return v as Record<string, unknown>;
  } catch {
    throw new QueryError("Send JSON.");
  }
}

function writer(c: C): void {
  if (c.get("claims").role === "viewer") throw new Forbidden("Your role can view this app but not change it.");
}

const pid = (c: C) => c.get("site").project_id;

export function insightsRoutes(deps: ServiceDeps): Hono<Env> {
  const app = new Hono<Env>();
  if (!deps.hooks.hooks.some((h) => h.name === cohortsGdprHook.name)) deps.hooks.register(cohortsGdprHook);
  const sandbox = new SqlSandbox(deps.db);
  const sqlRunning = new Set<string>();
  // The scheduler runs in the server process; test runners (and PA_INSIGHTS_JOBS=off) drive the tasks by hand.
  if (process.env.PA_INSIGHTS_JOBS !== "off" && !process.env.NODE_TEST_CONTEXT) startInsightsJobs(deps);

  app.use("/v1/projects/:id/insights/*", async (c, next) => {
    if (!deps.consoleSecret) return c.json({ error: "The console connection is not configured on this service." }, 503);
    const auth = c.req.header("authorization") ?? "";
    const claims = await verifyConsoleToken(deps.consoleSecret, auth.startsWith("Bearer ") ? auth.slice(7) : null, deps.now());
    if (!claims) return c.json({ error: "Sign in again." }, 401);
    const id = c.req.param("id");
    if (!isUuid(id) || !claims.app_ids.includes(id)) return c.json({ error: "This app is not in your organization." }, 403);
    const site = await deps.sites.byProject(id);
    if (!site || site.org_id !== claims.org_id) return c.json({ error: "This app is not in your organization." }, 403);
    c.set("claims", claims);
    c.set("site", site);
    await next();
  });

  app.onError((err, c) => {
    const status = (err as { status?: number }).status;
    if (err instanceof QueryError || err instanceof SqlRejected || err instanceof Forbidden || err instanceof TooMany || err instanceof SqlUnavailable || status === 400) {
      return c.json({ error: err.message }, (status ?? 400) as 400 | 403 | 429 | 503);
    }
    deps.log.error("insights request failed", { path: new URL(c.req.url).pathname, error: err.message });
    return c.json({ error: "Something went wrong. Try again shortly." }, 500);
  });

  const P = "/v1/projects/:id/insights";

  app.get(`${P}/meta`, async (c) => c.json(await projectMeta(deps.db, pid(c), deps.now())));

  app.post(`${P}/query`, async (c) => {
    const b = await body(c);
    const mode: CacheMode = b.refresh === true ? "force" : "fresh";
    return c.json(await computeCached(deps.db, pid(c), parseQuery(b.query), deps.now(), mode));
  });

  app.post(`${P}/funnel-people`, async (c) => {
    const b = await body(c);
    const q = parseQuery(b.query);
    if (q.kind !== "funnels") throw new QueryError("People lists are for funnels.");
    const bv = b.breakdown_value === undefined ? undefined : b.breakdown_value === null ? null : String(b.breakdown_value).slice(0, 400);
    return c.json({ people: await runFunnelPeople(deps.db, pid(c), q as FunnelsQuery, deps.now(), Number(b.step), b.dropped === true, bv) });
  });

  app.post(`${P}/time-to-convert`, async (c) => {
    const b = await body(c);
    const q = parseQuery(b.query);
    if (q.kind !== "funnels") throw new QueryError("Time to convert is for funnels.");
    const bins = typeof b.bins === "number" && Number.isInteger(b.bins) ? Math.min(60, Math.max(2, b.bins)) : 10;
    return c.json(await runTimeToConvert(deps.db, pid(c), q as FunnelsQuery, deps.now(), bins));
  });

  // Saved insights
  app.get(`${P}/saved`, async (c) => c.json({ insights: await listSavedInsights(deps.db, pid(c)) }));
  app.post(`${P}/saved`, async (c) => {
    writer(c);
    const b = await body(c);
    return c.json({ insight: await createSavedInsight(deps.db, pid(c), { name: b.name, query: b.query }) }, 201);
  });
  app.get(`${P}/saved/:iid`, async (c) => {
    const i = await getSavedInsight(deps.db, pid(c), c.req.param("iid"));
    return i ? c.json({ insight: i }) : c.json({ error: "No saved insight has that id in this app." }, 404);
  });
  app.put(`${P}/saved/:iid`, async (c) => {
    writer(c);
    const b = await body(c);
    const i = await updateSavedInsight(deps.db, pid(c), c.req.param("iid"), { name: b.name, query: b.query });
    return i ? c.json({ insight: i }) : c.json({ error: "No saved insight has that id in this app." }, 404);
  });
  app.delete(`${P}/saved/:iid`, async (c) => {
    writer(c);
    return (await deleteSavedInsight(deps.db, pid(c), c.req.param("iid"))) ? c.json({ deleted: true }) : c.json({ error: "No saved insight has that id in this app." }, 404);
  });

  // Dashboards
  app.get(`${P}/dashboards`, async (c) => c.json({ dashboards: await listDashboards(deps.db, pid(c)) }));
  app.post(`${P}/dashboards`, async (c) => {
    writer(c);
    const b = await body(c);
    return c.json({ dashboard: await createDashboard(deps.db, pid(c), { name: b.name }) }, 201);
  });
  app.get(`${P}/dashboards/:did`, async (c) => {
    const d = await getDashboard(deps.db, pid(c), c.req.param("did"), deps.now(), { refresh: c.req.query("refresh") === "1" });
    return d ? c.json({ dashboard: d }) : c.json({ error: "No dashboard has that id in this app." }, 404);
  });
  app.delete(`${P}/dashboards/:did`, async (c) => {
    writer(c);
    return (await deleteDashboard(deps.db, pid(c), c.req.param("did"))) ? c.json({ deleted: true }) : c.json({ error: "No dashboard has that id in this app." }, 404);
  });
  app.post(`${P}/dashboards/:did/tiles`, async (c) => {
    writer(c);
    const b = await body(c);
    await addTile(deps.db, pid(c), c.req.param("did"), String(b.insight_id ?? ""));
    return c.json({ added: true }, 201);
  });
  app.delete(`${P}/dashboards/:did/tiles/:iid`, async (c) => {
    writer(c);
    return (await removeTile(deps.db, pid(c), c.req.param("did"), c.req.param("iid"))) ? c.json({ removed: true }) : c.json({ error: "That insight is not on the dashboard." }, 404);
  });

  // Actions
  app.get(`${P}/actions`, async (c) => c.json({ actions: await listActions(deps.db, pid(c)) }));
  app.post(`${P}/actions`, async (c) => {
    writer(c);
    const b = await body(c);
    return c.json({ action: await createAction(deps.db, pid(c), { name: b.name, steps: b.steps }) }, 201);
  });
  app.put(`${P}/actions/:aid`, async (c) => {
    writer(c);
    const b = await body(c);
    const a = await updateAction(deps.db, pid(c), c.req.param("aid"), { name: b.name, steps: b.steps });
    return a ? c.json({ action: a }) : c.json({ error: "No action has that id in this app." }, 404);
  });
  app.delete(`${P}/actions/:aid`, async (c) => {
    writer(c);
    return (await deleteAction(deps.db, pid(c), c.req.param("aid"))) ? c.json({ deleted: true }) : c.json({ error: "No action has that id in this app." }, 404);
  });

  // Cohorts ("segments" in the console's words and in this API).
  app.get(`${P}/segments`, async (c) => c.json({ segments: await listCohorts(deps.db, pid(c)) }));
  app.post(`${P}/segments`, async (c) => {
    writer(c);
    const b = await body(c);
    const cohort = await createCohort(deps.db, pid(c), { name: b.name, kind: b.kind, definition: b.definition });
    const done = cohort.kind === "behavioural" ? await recomputeCohort(deps.db, pid(c), cohort.id, deps.now()) : cohort;
    return c.json({ segment: done }, 201);
  });
  app.get(`${P}/segments/:cid`, async (c) => {
    const k = await getCohort(deps.db, pid(c), c.req.param("cid"));
    return k ? c.json({ segment: k }) : c.json({ error: "No segment has that id in this app." }, 404);
  });
  app.delete(`${P}/segments/:cid`, async (c) => {
    writer(c);
    return (await deleteCohort(deps.db, pid(c), c.req.param("cid"))) ? c.json({ deleted: true }) : c.json({ error: "No segment has that id in this app." }, 404);
  });
  app.post(`${P}/segments/:cid/recompute`, async (c) => {
    writer(c);
    return c.json({ segment: await recomputeCohort(deps.db, pid(c), c.req.param("cid"), deps.now()) });
  });
  app.post(`${P}/segments/:cid/csv`, async (c) => {
    writer(c);
    const text = await c.req.text();
    if (Buffer.byteLength(text, "utf8") > MAX_CSV_BYTES) throw new QueryError("Keep the file under 5 MB.");
    return c.json({ segment: await importCohortCsv(deps.db, pid(c), c.req.param("cid"), text, deps.now()) });
  });

  // SQL
  const runSql = async (c: C) => {
    const b = await body(c);
    const project = pid(c);
    if (sqlRunning.has(project)) throw new TooMany("A query for this app is still running. Wait for it to finish.");
    sqlRunning.add(project);
    try {
      return await sandbox.run(project, typeof b.sql === "string" ? b.sql : "");
    } finally {
      sqlRunning.delete(project);
    }
  };
  app.post(`${P}/sql`, async (c) => c.json(await runSql(c)));
  app.post(`${P}/sql.csv`, async (c) => {
    const r = await runSql(c);
    return c.body(toCsv(r), 200, { "content-type": "text/csv; charset=utf-8", "content-disposition": 'attachment; filename="query.csv"', "cache-control": "no-store" });
  });

  return app;
}
