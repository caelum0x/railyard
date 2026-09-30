// /flags/?v=2 and /decide/: flag answers for one distinct_id, read from the
// flags slot (plain on/off plus value; variants come from variants.ts).
// A failure to read the slot answers errorsWhileComputingFlags: true with no
// flags, which the SDKs treat as "keep what you had".

import { randomUUID } from "node:crypto";
import { Hono, type Context } from "hono";
import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { CORS_HEADERS, problem, readPayload } from "../capture/routes.ts";
import { EventError, tokenOf } from "../capture/normalize.ts";
import { getSettings } from "../tenancy/settings.ts";
import type { SiteDirectory } from "../tenancy/site-directory.ts";
import type { FlagsReader, SlotFlag } from "./client.ts";
import { variantFor } from "./variants.ts";

export interface FlagsDeps {
  db: Db;
  sites: SiteDirectory;
  flags: FlagsReader;
  log: Logger;
}

export interface FlagAnswer {
  key: string;
  enabled: boolean;
  variant: string | null;
  payload: string | null;
}

export function answer(flags: Record<string, SlotFlag>, distinctId: string): FlagAnswer[] {
  return Object.entries(flags).map(([key, f]) => {
    const variant = f.enabled ? variantFor(key, f, distinctId) : null;
    return { key, enabled: f.enabled, variant, payload: f.enabled ? f.value : null };
  });
}

export function v2Body(list: FlagAnswer[], errors: boolean) {
  return {
    flags: Object.fromEntries(
      list.map((a) => [
        a.key,
        {
          key: a.key,
          enabled: a.enabled,
          variant: a.variant,
          reason: { code: a.enabled ? "condition_match" : "no_condition_match", description: a.enabled ? "On" : "Off", condition_index: a.enabled ? 0 : null },
          metadata: { id: 0, version: 0, payload: a.payload, description: null },
        },
      ]),
    ),
    errorsWhileComputingFlags: errors,
    requestId: randomUUID(),
  };
}

export function legacyBody(list: FlagAnswer[], errors: boolean) {
  return {
    featureFlags: Object.fromEntries(list.filter((a) => a.enabled).map((a) => [a.key, a.variant ?? true])),
    featureFlagPayloads: Object.fromEntries(list.filter((a) => a.enabled && a.payload !== null).map((a) => [a.key, a.payload])),
    errorsWhileComputingFlags: errors,
  };
}

export function flagsRoutes(deps: FlagsDeps): Hono {
  const app = new Hono();
  const handle = (legacy: boolean) => async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = (await readPayload(c)) as Record<string, unknown>;
      const token = tokenOf(payload, null, new URL(c.req.url).searchParams.get("token"));
      if (!token) throw new EventError(401, "The project token is missing.");
      const site = await deps.sites.byToken(token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      const distinctId = payload?.distinct_id;
      if (typeof distinctId !== "string" && typeof distinctId !== "number") throw new EventError(400, "distinct_id is required.");
      const settings = await getSettings(deps.db, site.project_id);
      let list: FlagAnswer[] = [];
      let errors = false;
      if (settings.flags_client_key) {
        try {
          list = answer(await deps.flags.read(settings.flags_client_key), String(distinctId));
        } catch (err) {
          errors = true;
          deps.log.error("flags slot read failed", { project_id: site.project_id, error: (err as Error).message });
        }
      }
      const body = legacy ? { ...legacyBody(list, errors), ...v2Body(list, errors) } : v2Body(list, errors);
      return c.json(body, 200, CORS_HEADERS(origin));
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };
  for (const [p, legacy] of [["/flags", false], ["/decide", true]] as const) {
    for (const path of [p, `${p}/`]) {
      app.options(path, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
      app.get(path, handle(legacy));
      app.post(path, handle(legacy));
    }
  }
  return app;
}
