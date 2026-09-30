// The capture endpoints of the SDK wire protocol:
//   /e/, /i/v0/e/, /batch/, /capture/   events (GET or POST)
//   /engage/                            person properties without an event
//   /groups/                            group properties without an event
// An unknown token is a 401. Over the cap or paused is still a 200, so SDKs
// do not retry; the console says what happened.

import { Hono, type Context } from "hono";
import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { eventOps } from "../persons/properties.ts";
import { updatePersonProperties } from "../persons/service.ts";
import { groupTypeIndex, upsertGroup } from "../groups/groups.ts";
import type { SiteDirectory } from "../tenancy/site-directory.ts";
import { DecodeError, decodePayload, MAX_BODY_BYTES } from "./decode.ts";
import { EventError, normalizeEvents, tokenOf, withoutIp } from "./normalize.ts";
import type { CapturePipeline } from "./pipeline.ts";

export interface CaptureDeps {
  db: Db;
  sites: SiteDirectory;
  pipeline: CapturePipeline;
  log: Logger;
  now: () => Date;
}

export const CORS_HEADERS = (origin: string | undefined): Record<string, string> => ({
  "access-control-allow-origin": origin ?? "*",
  "access-control-allow-credentials": origin ? "true" : "false",
  "access-control-allow-methods": "GET, POST, OPTIONS",
  "access-control-allow-headers": "content-type, content-encoding, authorization",
  "access-control-max-age": "86400",
  vary: "origin",
});

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => typeof v === "object" && v !== null && !Array.isArray(v);

export async function readPayload(c: Context): Promise<unknown> {
  const len = Number(c.req.header("content-length") ?? "0");
  if (len > MAX_BODY_BYTES) throw new DecodeError(413, "The body is too large.");
  const body = c.req.method === "GET" ? new Uint8Array() : new Uint8Array(await c.req.arrayBuffer());
  return decodePayload({
    method: c.req.method,
    url: new URL(c.req.url),
    contentType: c.req.header("content-type") ?? null,
    contentEncoding: c.req.header("content-encoding") ?? null,
    body,
  });
}

export function problem(c: Context, err: unknown, log: Logger) {
  const origin = c.req.header("origin");
  if (err instanceof DecodeError || err instanceof EventError) {
    return c.json({ type: err.status === 401 ? "authentication_error" : "validation_error", detail: err.message }, err.status, CORS_HEADERS(origin));
  }
  log.error("request failed", { path: new URL(c.req.url).pathname, error: (err as Error).message });
  return c.json({ type: "server_error", detail: "The event could not be processed. Try again shortly." }, 503, CORS_HEADERS(origin));
}

export function captureRoutes(deps: CaptureDeps): Hono {
  const app = new Hono();
  const paths = ["/e", "/e/", "/i/v0/e", "/i/v0/e/", "/batch", "/batch/", "/capture", "/capture/", "/track", "/track/"];

  const capture = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      const url = new URL(c.req.url);
      const sentAtQuery = Number(url.searchParams.get("_"));
      const n = normalizeEvents(payload, {
        now: deps.now(),
        country: c.req.header("cf-ipcountry") ?? null,
        queryToken: url.searchParams.get("token") ?? url.searchParams.get("api_key"),
        sentAtQuery: Number.isFinite(sentAtQuery) && sentAtQuery > 0 ? sentAtQuery : null,
      });
      const site = await deps.sites.byToken(n.token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      await deps.pipeline.ingest(site, n.events);
      return c.json({ status: 1 }, 200, CORS_HEADERS(origin));
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };
  for (const p of paths) {
    app.options(p, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
    app.get(p, capture);
    app.post(p, capture);
  }

  const engage = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      const list = Array.isArray(payload) ? payload : [payload];
      if (list.length === 0 || list.length > 1000 || !list.every(isObj)) throw new EventError(400, "Send one update or a list of updates.");
      const token = tokenOf(list[0], null, new URL(c.req.url).searchParams.get("token"));
      if (!token) throw new EventError(401, "The project token is missing.");
      const site = await deps.sites.byToken(token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      const updates = (list as Obj[]).map((u, i) => {
        const id = u.$distinct_id ?? u.distinct_id;
        if (typeof id !== "string" || !id || id.length > 200) throw new EventError(400, `Update ${i} needs a distinct_id.`);
        if ((u.$token ?? u.token ?? u.api_key ?? token) !== token) throw new EventError(400, "Every update of one request must use the same project token.");
        const unset = Array.isArray(u.$unset) ? u.$unset.filter((k): k is string => typeof k === "string") : null;
        return { id, ops: eventOps({ event: "$engage", properties: {}, set: isObj(u.$set) ? withoutIp(u.$set) : null, set_once: isObj(u.$set_once) ? withoutIp(u.$set_once) : null, unset }) };
      });
      if (!site.paused) {
        await deps.pipeline.serial(`org:${site.org_id}`, () =>
          deps.db.asProject(site.project_id, async (tx) => {
            for (const u of updates) await updatePersonProperties(tx, site.project_id, u.id, u.ops, deps.now());
          }),
        );
      }
      return c.json({ status: 1 }, 200, CORS_HEADERS(origin));
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };

  const groups = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      if (!isObj(payload)) throw new EventError(400, "Send one group update.");
      const token = tokenOf(payload, null, new URL(c.req.url).searchParams.get("token"));
      if (!token) throw new EventError(401, "The project token is missing.");
      const site = await deps.sites.byToken(token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      const type = payload.$group_type ?? payload.group_type;
      const key = payload.$group_key ?? payload.group_key;
      const set = payload.$group_set ?? payload.properties ?? {};
      if (typeof type !== "string" || !type || type.length > 100) throw new EventError(400, "group_type is required.");
      if ((typeof key !== "string" && typeof key !== "number") || !String(key) || String(key).length > 400) throw new EventError(400, "group_key is required.");
      if (!isObj(set)) throw new EventError(400, "Group properties must be an object.");
      if (!site.paused) {
        await deps.pipeline.serial(`org:${site.org_id}`, () =>
          deps.db.asProject(site.project_id, async (tx) => {
            const idx = await groupTypeIndex(tx, site.project_id, type, new Map());
            if (idx !== null) await upsertGroup(tx, site.project_id, idx, String(key), withoutIp(set), deps.now());
          }),
        );
      }
      return c.json({ status: 1 }, 200, CORS_HEADERS(origin));
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };

  for (const [p, h] of [["/engage", engage], ["/groups", groups]] as const) {
    for (const path of [p, `${p}/`]) {
      app.options(path, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
      app.get(path, h);
      app.post(path, h);
    }
  }
  return app;
}
