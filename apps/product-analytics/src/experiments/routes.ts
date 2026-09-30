// Router for experiments on the flags slot. Owned by A3 (handed over by A1: "then").
// Mounted at the service root by src/routes.ts; A3 adds its routes here.

import { Hono } from "hono";
import type { ServiceDeps } from "../app-deps.ts";

export function experimentsRoutes(_deps: ServiceDeps): Hono {
  return new Hono();
}
