// The capture endpoints of the SDK wire protocol:
//   /e/, /i/v0/e/, /batch/, /capture/   events (GET or POST)
//   /engage/                            person properties without an event
//   /groups/                            group properties without an event
// An unknown token is a 401. Over the cap or paused is still a 200, so SDKs
// do not retry; the console says what happened.

import { Hono, type Context } from "hono";
import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { logPath } from "../log.ts";
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

// Mixpanel's real wire contract: a client that does not opt into `verbose`
// (the mixpanel-node/mixpanel-js/mixpanel-python SDKs all default to this,
// and still send an explicit `verbose=0` -- never nothing) treats the
// response body as a failure unless it is the literal text "1" (or "0" on
// failure); only `verbose=1` switches the response to JSON with a `status`
// field. This service's own PostHog-shaped SDKs never send a `verbose`
// query param at all, so a request with no `verbose` param keeps today's
// unconditional JSON response -- only a request that explicitly opts into
// Mixpanel's contract (by sending `verbose` at all) gets the plain-text
// behavior when it asks for it.
type ResponseMode = "json" | "plain";
function responseMode(url: URL): ResponseMode {
  const v = url.searchParams.get("verbose");
  if (v === null) return "json";
  return v === "1" || v === "true" ? "json" : "plain";
}

/** Success response honoring the caller's verbose/plain contract. `extra` is only visible in JSON mode -- the plain "1" contract carries no payload. */
function okResponse(c: Context, origin: string | undefined, url: URL, extra: Record<string, unknown> = {}) {
  const headers = CORS_HEADERS(origin);
  if (responseMode(url) === "plain") return c.text("1", 200, headers);
  return c.json({ status: 1, ...extra }, 200, headers);
}

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
  const plain = responseMode(new URL(c.req.url)) === "plain";
  if (err instanceof DecodeError || err instanceof EventError) {
    if (plain) return c.text("0", err.status, CORS_HEADERS(origin));
    return c.json({ type: err.status === 401 ? "authentication_error" : "validation_error", detail: err.message }, err.status, CORS_HEADERS(origin));
  }
  log.error("request failed", { path: logPath(c.req.url), error: (err as Error).message });
  if (plain) return c.text("0", 503, CORS_HEADERS(origin));
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
      return okResponse(c, origin, url);
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };
  for (const p of paths) {
    app.options(p, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
    app.get(p, capture);
    app.post(p, capture);
  }

  /**
   * Historical/batch import, separate from /track's live path:
   *  - auth: Mixpanel's real /import requires a service account or API
   *    secret (Basic auth), not the bare project token every SDK event
   *    already carries in its body for /track. This service has no
   *    separate secret, so it accepts the project token as Basic auth or as
   *    an explicit api_key/token query parameter instead -- never from the
   *    event body alone.
   *  - retention: /track silently drops events older than the plan's
   *    retention window (that's the live path's job); /import exists to
   *    backfill events from before that window, so it skips the cutoff.
   *  - idempotency: shared with /track via the same uuid/$insert_id
   *    dedup (../capture/writer.ts's ingest_receipts) -- importing the same
   *    event twice stores it once.
   */
  const importRoute = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      const url = new URL(c.req.url);
      const auth = c.req.header("authorization");
      let basicToken: string | null = null;
      if (auth && /^basic\s+/i.test(auth)) {
        const decoded = Buffer.from(auth.replace(/^basic\s+/i, ""), "base64").toString("utf8");
        const sep = decoded.indexOf(":");
        const user = sep === -1 ? decoded : decoded.slice(0, sep);
        const pass = sep === -1 ? "" : decoded.slice(sep + 1);
        basicToken = pass || user || null;
      }
      const queryToken = url.searchParams.get("token") ?? url.searchParams.get("api_key");
      const strongToken = basicToken ?? queryToken;
      if (!strongToken) throw new EventError(401, "/import requires Basic authentication (service account or API secret) or an api_key/token query parameter; a token inside the event body alone is not enough.");
      const site = await deps.sites.byToken(strongToken);
      if (!site) throw new EventError(401, "The project token is not valid.");
      const n = normalizeEvents(payload, { now: deps.now(), country: c.req.header("cf-ipcountry") ?? null, queryToken: strongToken, sentAtQuery: null });
      if (n.token !== site.token) throw new EventError(400, "Every event of one request must use the same project token as the import credential.");
      const result = await deps.pipeline.ingest(site, n.events, { historical: true });
      return okResponse(c, origin, url, { num_records_imported: result.accepted });
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };
  for (const p of ["/import", "/import/"]) {
    app.options(p, (c) => c.body(null, 204, CORS_HEADERS(c.req.header("origin"))));
    app.get(p, importRoute);
    app.post(p, importRoute);
  }

  const engage = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      const url = new URL(c.req.url);
      const list = Array.isArray(payload) ? payload : [payload];
      if (list.length === 0 || list.length > 1000 || !list.every(isObj)) throw new EventError(400, "Send one update or a list of updates.");
      const token = tokenOf(list[0], null, url.searchParams.get("token"));
      if (!token) throw new EventError(401, "The project token is missing.");
      const site = await deps.sites.byToken(token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      const updates = (list as Obj[]).map((u, i) => {
        const id = u.$distinct_id ?? u.distinct_id;
        if (typeof id !== "string" || !id || id.length > 200) throw new EventError(400, `Update ${i} needs a distinct_id.`);
        if ((u.$token ?? u.token ?? u.api_key ?? token) !== token) throw new EventError(400, "Every update of one request must use the same project token.");
        const unset = Array.isArray(u.$unset) ? u.$unset.filter((k): k is string => typeof k === "string") : null;
        return {
          id,
          ops: eventOps({
            event: "$engage",
            properties: {},
            set: isObj(u.$set) ? withoutIp(u.$set) : null,
            set_once: isObj(u.$set_once) ? withoutIp(u.$set_once) : null,
            unset,
            add: isObj(u.$add) ? u.$add : null,
            append: isObj(u.$append) ? withoutIp(u.$append) : null,
            union: isObj(u.$union) ? u.$union : null,
            remove: isObj(u.$remove) ? u.$remove : null,
          }),
        };
      });
      if (!site.paused) {
        await deps.pipeline.serial(`org:${site.org_id}`, () =>
          deps.db.asProject(site.project_id, async (tx) => {
            for (const u of updates) await updatePersonProperties(tx, site.project_id, u.id, u.ops, deps.now());
          }),
        );
      }
      return okResponse(c, origin, url);
    } catch (err) {
      return problem(c, err, deps.log);
    }
  };

  const groups = async (c: Context) => {
    const origin = c.req.header("origin");
    try {
      const payload = await readPayload(c);
      const url = new URL(c.req.url);
      if (!isObj(payload)) throw new EventError(400, "Send one group update.");
      const token = tokenOf(payload, null, url.searchParams.get("token"));
      if (!token) throw new EventError(401, "The project token is missing.");
      const site = await deps.sites.byToken(token);
      if (!site) throw new EventError(401, "The project token is not valid.");
      // This service's own wire protocol uses $group_type (the group's TYPE,
      // e.g. "company") and $group_key (the group's KEY, e.g. "acme"). The
      // real Mixpanel SDK instead sends $group_key for the TYPE and
      // $group_id for the KEY -- the opposite meaning for $group_key, plus a
      // field ($group_id) this service never otherwise reads. $group_id only
      // exists in Mixpanel's shape, so its presence disambiguates which
      // protocol sent the request.
      const mixpanel = payload.$group_id !== undefined;
      const type = mixpanel ? payload.$group_key : payload.$group_type ?? payload.group_type;
      const key = mixpanel ? payload.$group_id : payload.$group_key ?? payload.group_key;
      if (typeof type !== "string" || !type || type.length > 100) throw new EventError(400, "group_type is required.");
      if ((typeof key !== "string" && typeof key !== "number") || !String(key) || String(key).length > 400) throw new EventError(400, "group_key is required.");
      const setSrc = payload.$set ?? payload.$group_set ?? payload.properties ?? {};
      if (!isObj(setSrc)) throw new EventError(400, "Group properties must be an object.");
      for (const [field, v] of [
        ["$set_once", payload.$set_once],
        ["$add", payload.$add],
        ["$append", payload.$append],
        ["$union", payload.$union],
        ["$remove", payload.$remove],
      ] as const) {
        if (v !== undefined && !isObj(v)) throw new EventError(400, `${field} must be an object.`);
      }
      const set = setSrc;
      const setOnce = isObj(payload.$set_once) ? payload.$set_once : null;
      const unset = Array.isArray(payload.$unset) ? payload.$unset.filter((k): k is string => typeof k === "string") : null;
      const add = isObj(payload.$add) ? payload.$add : null;
      const append = isObj(payload.$append) ? payload.$append : null;
      const union = isObj(payload.$union) ? payload.$union : null;
      const remove = isObj(payload.$remove) ? payload.$remove : null;
      const ops = eventOps({
        event: "$groups",
        properties: {},
        set: withoutIp(set),
        set_once: setOnce ? withoutIp(setOnce) : null,
        unset,
        add,
        append: append ? withoutIp(append) : null,
        union,
        remove,
      });
      if (!site.paused) {
        await deps.pipeline.serial(`org:${site.org_id}`, () =>
          deps.db.asProject(site.project_id, async (tx) => {
            const idx = await groupTypeIndex(tx, site.project_id, type, new Map());
            if (idx !== null) await upsertGroup(tx, site.project_id, idx, String(key), ops, deps.now());
          }),
        );
      }
      return okResponse(c, origin, url);
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
