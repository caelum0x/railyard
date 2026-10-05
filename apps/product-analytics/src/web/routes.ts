// Web analytics and the server-side capture door (QY1.2).
//   Console: GET  /v1/projects/:id/web?from&to|days&limit   the web view (see query.ts)
//   Server:  POST /v1/server/:id/batch                    events from a server forwarder,
//            Authorization: Bearer ry_pak_... (server-key.ts); the same pipeline,
//            caps and privacy rules as the public capture endpoints.

import { Hono } from "hono";
import type { ServiceDeps } from "../app-deps.ts";
import { CORS_HEADERS, problem, readPayload } from "../capture/routes.ts";
import { EventError, normalizeEvents } from "../capture/normalize.ts";
import { isUuid } from "../db.ts";
import { consoleGuard, plainErrors, projectOf, type ConsoleEnv } from "../replay/shared/console-guard.ts";
import { parseWebQuery, webOverview } from "./query.ts";
import { verifyServerKey } from "./server-key.ts";

export function webRoutes(deps: ServiceDeps): Hono {
  const app = new Hono();

  app.post("/v1/server/:id/batch", async (c) => {
    try {
      const id = c.req.param("id");
      const auth = c.req.header("authorization") ?? "";
      if (!isUuid(id) || !verifyServerKey(deps.consoleSecret, id, auth.startsWith("Bearer ") ? auth.slice(7) : null)) {
        throw new EventError(401, "The server key is not valid for this project.");
      }
      const site = await deps.sites.byProject(id);
      if (!site) throw new EventError(401, "The server key is not valid for this project.");
      const payload = await readPayload(c);
      const n = normalizeEvents(payload, { now: deps.now(), country: null, queryToken: site.token, sentAtQuery: null });
      if (n.token !== site.token) throw new EventError(400, "Leave the project token out; the server key names the project.");
      await deps.pipeline.ingest(site, n.events);
      return c.json({ status: 1, received: n.events.length });
    } catch (err) {
      return problem(c, err, deps.log);
    }
  });
  app.options("/v1/server/:id/batch", (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));

  const api = new Hono<ConsoleEnv>();
  const guard = consoleGuard(deps);
  api.use("/v1/projects/:id/web", guard);
  plainErrors(api, deps.log, "web");
  api.get("/v1/projects/:id/web", async (c) => c.json(await webOverview(deps.db, projectOf(c), parseWebQuery(new URL(c.req.url).searchParams, deps.now()))));
  app.route("/", api);
  return app;
}
