// POST /v1/bridge/scribase/proof (Standard Webhooks, secret
// RAILYARD_SCRIBASE_BRIDGE_SECRET like the console bridge). Body:
//   {"slot": "analytics", "org_id": "<railyard org uuid>"} or
//   {"slot": "analytics", "scribase_org_id": "<scribase org id>"}
// Answers the console's slot shape with proof kind `first_product_event`:
//   {slot, state, live, proof: {kind, id: <app id>, at}, next}

import { Hono } from "hono";
import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import type { Logger } from "../log.ts";
import type { SiteDirectory } from "../tenancy/site-directory.ts";
import { verify } from "./standard-webhooks.ts";

const SCRIBASE_ORG = /^[A-Za-z0-9_-]{1,128}$/;

export interface ProofDeps {
  db: Db;
  sites: SiteDirectory;
  secret: string | null;
  log: Logger;
  now: () => Date;
}

export async function firstProductEvent(db: Db, projectIds: string[]): Promise<{ project_id: string; at: string } | null> {
  if (projectIds.length === 0) return null;
  const r = await db.query<{ project_id: string; at: Date }>(
    "select project_id, at from pa.project_firsts where project_id = any($1::uuid[]) order by at limit 1",
    [projectIds],
  );
  return r[0] ? { project_id: r[0].project_id, at: new Date(r[0].at).toISOString() } : null;
}

export function proofRoutes(deps: ProofDeps): Hono {
  const app = new Hono();
  app.post("/v1/bridge/scribase/proof", async (c) => {
    if (!deps.secret) return c.json({ error: "bridge_not_configured" }, 503);
    const body = await c.req.text();
    const ok = verify(
      deps.secret,
      { id: c.req.header("webhook-id") ?? null, timestamp: c.req.header("webhook-timestamp") ?? null, signature: c.req.header("webhook-signature") ?? null },
      body,
      Math.floor(deps.now().getTime() / 1000),
    );
    if (!ok) return c.json({ error: "invalid_signature" }, 401);
    let input: Record<string, unknown>;
    try {
      input = JSON.parse(body);
    } catch {
      return c.json({ error: "body must be JSON" }, 400);
    }
    if (input?.slot !== undefined && input.slot !== "analytics") return c.json({ error: "slot must be analytics" }, 400);
    let sites;
    if (isUuid(input?.org_id)) sites = await deps.sites.byOrg(input.org_id);
    else if (typeof input?.scribase_org_id === "string" && SCRIBASE_ORG.test(input.scribase_org_id)) sites = await deps.sites.byScribaseOrg(input.scribase_org_id);
    else return c.json({ error: "org_id or scribase_org_id is required" }, 400);
    if (sites.length === 0) {
      return c.json({ slot: "analytics", state: "not_entitled", live: false, proof: null, next: "Add an app first; its events appear here." });
    }
    try {
      const first = await firstProductEvent(deps.db, sites.map((s) => s.project_id));
      return c.json(
        first
          ? { slot: "analytics", state: "live", live: true, proof: { kind: "first_product_event", id: first.project_id, at: first.at }, next: null }
          : { slot: "analytics", state: "no_proof_yet", live: true, proof: null, next: "Waiting for the first event from the app." },
      );
    } catch (err) {
      deps.log.error("proof read failed", { error: (err as Error).message });
      return c.json({ error: "Try again shortly." }, 503);
    }
  });
  return app;
}
