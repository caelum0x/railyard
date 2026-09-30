// Router for recordings (/s/ ingest, chunk index, player). Owned by A3 (handed over by A1: "then").
// Mounted at the service root by src/routes.ts; A3 adds its routes here.

import { Hono } from "hono";
import type { ServiceDeps } from "../app-deps.ts";

export function replayRoutes(_deps: ServiceDeps): Hono {
  return new Hono();
}
