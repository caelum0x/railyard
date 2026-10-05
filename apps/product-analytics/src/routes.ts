// Every router of the service. A1 owns capture, flags, remote config,
// console, bridge and privacy; the mount lines below hand the rest over:
//   insights    -> A2 (src/insights/routes.ts)
//   replay, heatmaps, surveys, experiments, exports -> A3

import { Hono } from "hono";
import type { ServiceDeps } from "./app-deps.ts";
import { captureRoutes } from "./capture/routes.ts";
import { consoleRoutes } from "./console/routes.ts";
import { flagsRoutes } from "./flags/routes.ts";
import { proofRoutes } from "./bridge/proof.ts";
import { remoteConfigRoutes } from "./remote-config/routes.ts";
import { SECTION_PROVIDERS } from "./remote-config/sections.ts";
import { insightsRoutes } from "./insights/routes.ts";
import { replayRoutes } from "./replay/routes.ts";
import { heatmapsRoutes } from "./heatmaps/routes.ts";
import { surveysRoutes } from "./surveys/routes.ts";
import { experimentsRoutes } from "./experiments/routes.ts";
import { exportsRoutes } from "./exports/routes.ts";
import { webRoutes } from "./web/routes.ts";
import { alertsRoutes } from "./alerts/routes.ts";

export function mountRoutes(app: Hono, deps: ServiceDeps): Hono {
  app.get("/healthz", (c) => c.json({ ok: true }));
  app.route("/", captureRoutes(deps));
  app.route("/", flagsRoutes(deps));
  app.route("/", remoteConfigRoutes({ ...deps, sections: SECTION_PROVIDERS }));
  app.route("/", consoleRoutes({ ...deps, secret: deps.consoleSecret }));
  app.route("/", proofRoutes({ ...deps, secret: deps.bridgeSecret }));
  app.route("/", insightsRoutes(deps)); // A2
  app.route("/", replayRoutes(deps)); // A3
  app.route("/", heatmapsRoutes(deps)); // A3
  app.route("/", surveysRoutes(deps)); // A3
  app.route("/", experimentsRoutes(deps)); // A3
  app.route("/", exportsRoutes(deps)); // A3
  app.route("/", webRoutes(deps)); // QY1.2: web view and server-side capture
  app.route("/", alertsRoutes(deps)); // QY1.4: alerts, subscriptions, unsubscribe
  return app;
}
