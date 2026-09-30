// Router for insights (trends, funnels, retention, paths, stickiness, lifecycle, cohorts, dashboards, SQL). Owned by A2 (handed over by A1: "then").
// Mounted at the service root by src/routes.ts; A2 adds its routes here.

import { Hono } from "hono";
import type { ServiceDeps } from "../app-deps.ts";

export function insightsRoutes(_deps: ServiceDeps): Hono {
  return new Hono();
}
