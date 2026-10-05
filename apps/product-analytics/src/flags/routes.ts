// /flags/?v=2 and /decide/: flag answers for one distinct_id, read from the
// flags slot (plain on/off plus value; variants come from variants.ts).
// Targeted flags (QY1.3) run their rules here with the person's stored
// properties (overridden by the request's person_properties, as the SDKs
// send them) and the cohorts the person is in, through targeting.mjs: the
// same code and hash as the console's read endpoint and the SDK providers.
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
import { evaluateAnswer, type SegmentMap, type TargetingContext } from "./targeting.mjs";

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

export function answer(flags: Record<string, SlotFlag>, distinctId: string, ctx: Omit<TargetingContext, "id"> = {}, segments: SegmentMap = {}): FlagAnswer[] {
  return Object.entries(flags).map(([key, f]) => {
    const r = evaluateAnswer(key, f, { ...ctx, id: distinctId }, segments);
    const served: SlotFlag = { enabled: r.enabled, value: r.value };
    const variant = r.enabled ? variantFor(key, served, distinctId) : null;
    return { key, enabled: r.enabled, variant, payload: r.enabled ? r.value : null };
  });
}

/** The person's stored properties and cohorts (only read when a flag has rules). */
export async function personContext(db: Db, projectId: string, distinctId: string): Promise<{ properties: Record<string, unknown>; cohorts: string[] }> {
  return db.asProject(projectId, async (c) => {
    const p = await c.query<{ id: string; properties: Record<string, unknown> }>(
      `select p.id, p.properties
         from pa.person_distinct_ids d join pa.persons p on p.id = d.person_id and p.project_id = d.project_id
        where d.project_id = $1 and d.distinct_id = $2`,
      [projectId, distinctId],
    );
    const person = p.rows[0];
    if (!person) return { properties: {}, cohorts: [] };
    const co = await c.query<{ cohort_id: string }>(
      `select distinct cp.cohort_id
         from pa.cohort_people cp
         left join pa.person_overrides o on o.project_id = cp.project_id and o.old_person_id = cp.person_id
        where cp.project_id = $1 and coalesce(o.new_person_id, cp.person_id) = $2`,
      [projectId, person.id],
    );
    return { properties: person.properties ?? {}, cohorts: co.rows.map((r) => r.cohort_id) };
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
          const env = deps.flags.readEnv ? await deps.flags.readEnv(settings.flags_client_key) : { flags: await deps.flags.read(settings.flags_client_key), segments: {} };
          let ctx: Omit<TargetingContext, "id"> = {};
          if (Object.values(env.flags).some((f) => f.targeting)) {
            const stored = await personContext(deps.db, site.project_id, String(distinctId));
            const given = payload?.person_properties;
            const overrides = given && typeof given === "object" && !Array.isArray(given) ? (given as Record<string, unknown>) : {};
            ctx = { properties: { ...stored.properties, ...overrides }, cohorts: stored.cohorts };
          }
          list = answer(env.flags, String(distinctId), ctx, env.segments);
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
