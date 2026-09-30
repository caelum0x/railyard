// Exports (A3; handed over by A1), under /v1/projects/:id/exports (console token).
//   destinations: list, create (a webhook's signing secret is shown once), read,
//                 pause or resume or retry now, delete, test (a signed ping), runs
//   downloads:    insight.csv {query}, saved/:iid.csv (an insight's answer as CSV);
//                 the SQL screen's CSV is the insights API's own sql.csv
// Delivery runs in the background (jobs.ts).

import { randomUUID } from "node:crypto";
import { Hono } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { computeCached } from "../insights/cache.ts";
import { getSavedInsight } from "../insights/store.ts";
import { QueryError } from "../insights/types.ts";
import { parseQuery } from "../insights/validate.ts";
import { ApiError, consoleGuard, jsonBody, plainErrors, projectOf, requireWriter, uuidParam, type ConsoleEnv } from "../replay/shared/console-guard.ts";
import { insightCsv } from "./csv.ts";
import { checkDestination, createDestination, deleteDestination, getDestination, listDestinations, listRuns, MAX_DESTINATIONS, updateDestination, type WebhookConfig } from "./destinations.ts";
import { startA3Jobs } from "./jobs.ts";
import { DeliveryError, sendWebhook } from "./runner.ts";
import { DestinationRefused } from "./net-guard.ts";
import { open } from "./secrets.ts";

const csvHeaders = (name: string) => ({ "content-type": "text/csv; charset=utf-8", "content-disposition": `attachment; filename="${name}.csv"`, "cache-control": "no-store" });

export function exportsRoutes(deps: ServiceDeps): Hono {
  startA3Jobs(deps);
  const api = new Hono<ConsoleEnv>();
  const P = "/v1/projects/:id/exports";
  const guard = consoleGuard(deps);
  api.use(P, guard);
  api.use(`${P}/*`, guard);
  plainErrors(api, deps.log, "exports");

  api.get(P, async (c) => c.json({ destinations: await listDestinations(deps.db, projectOf(c)), max_destinations: MAX_DESTINATIONS }));

  api.post(P, async (c) => {
    requireWriter(c);
    const input = checkDestination(await jsonBody(c));
    const destination = await createDestination(deps.db, projectOf(c), input, deps.now());
    // The signing secret is shown once, now; it is never read back.
    return c.json({ destination, signing_secret: input.kind === "webhook" ? input.secret : null }, 201);
  });

  api.post(`${P}/insight.csv`, async (c) => {
    const b = await jsonBody(c);
    try {
      const answer = await computeCached(deps.db, projectOf(c), parseQuery(b.query), deps.now(), "prefer_cache");
      return c.body(insightCsv(answer.result), 200, csvHeaders("insight"));
    } catch (err) {
      if (err instanceof QueryError) throw new ApiError(400, err.message);
      throw err;
    }
  });

  api.get(`${P}/saved/:iid{[0-9a-f-]{36}\\.csv}`, async (c) => {
    const id = (c.req.param("iid") ?? "").replace(/\.csv$/, "");
    const saved = await getSavedInsight(deps.db, projectOf(c), id);
    if (!saved) throw new ApiError(404, "That insight does not exist.");
    const answer = await computeCached(deps.db, projectOf(c), parseQuery(saved.query), deps.now(), "prefer_cache");
    return c.body(insightCsv(answer.result), 200, csvHeaders(saved.name.replace(/[^A-Za-z0-9 _-]/g, "").trim().replace(/\s+/g, "-").slice(0, 60) || "insight"));
  });

  api.get(`${P}/:did`, async (c) => {
    const id = uuidParam(c, "did", "destination");
    return c.json({ destination: await deps.db.asProject(projectOf(c), (tx) => getDestination(tx, projectOf(c), id)) });
  });

  api.put(`${P}/:did`, async (c) => {
    requireWriter(c);
    return c.json({ destination: await updateDestination(deps.db, projectOf(c), uuidParam(c, "did", "destination"), await jsonBody(c), deps.now()) });
  });

  api.delete(`${P}/:did`, async (c) => {
    requireWriter(c);
    await deleteDestination(deps.db, projectOf(c), uuidParam(c, "did", "destination"));
    return c.json({ deleted: true });
  });

  api.get(`${P}/:did/runs`, async (c) => c.json({ runs: await listRuns(deps.db, projectOf(c), uuidParam(c, "did", "destination")) }));

  api.post(`${P}/:did/test`, async (c) => {
    requireWriter(c);
    const id = uuidParam(c, "did", "destination");
    const raw = await deps.db.asProject(projectOf(c), async (tx) => (await tx.query("select kind, config, secret_enc from pa.export_destinations where project_id = $1 and id = $2", [projectOf(c), id])).rows[0]);
    if (!raw) throw new ApiError(404, "That destination does not exist.");
    if (raw.kind !== "webhook") throw new ApiError(400, "A test ping is for webhooks. A bucket is checked by its first delivery.");
    const batch = randomUUID();
    try {
      const status = await sendWebhook(raw.config as WebhookConfig, open(raw.secret_enc), JSON.stringify({ type: "ping", project_id: projectOf(c), batch_id: batch, events: [] }), `msg_${batch}`, deps.now(), fetch);
      return c.json({ ok: true, status });
    } catch (err) {
      if (err instanceof DeliveryError || err instanceof DestinationRefused) return c.json({ ok: false, error: err.message });
      throw err;
    }
  });

  const app = new Hono();
  app.route("/", api);
  return app;
}
