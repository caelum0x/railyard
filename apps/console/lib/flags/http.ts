// HTTP handlers for Railyard Flags (Wave P PY2.1), framework-free so the
// route files stay three lines and the tests call them directly.

import type { Repo } from "../repo.ts";
import { deleteSegment, ensureEnv, evaluate, EVAL_MAX_AGE_SECONDS, FlagsError, listFlags, listSegments, setFlag, setSegment, type FlagsDeps } from "./service.ts";
import type { TargetingContext } from "./targeting.mjs";

export const CORS: Record<string, string> = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET, OPTIONS",
  "access-control-allow-headers": "x-flags-key, content-type",
  "access-control-max-age": "86400",
};

const reply = (body: unknown, status: number, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });

/** The slice of the Workers Cache API the read endpoint uses. */
export interface EdgeCache {
  match(request: Request): Promise<Response | undefined>;
  put(request: Request, response: Response): Promise<void>;
}

/**
 * GET /api/v1/flags/client?key=ry_flags_... (or header x-flags-key): the
 * environment's flags for an app. Public, CORS open. Optional context for
 * targeted flags (QY1.3): `id` (the person), `props` (a JSON object of the
 * person's properties, 2 KB at most) and `cohorts` (comma-separated ids). A URL-keyed answer is
 * kept 15 s in the edge cache (the Workers Cache API when `cache` is given),
 * so a flip reaches every app within 60 s while polling stays cheap.
 */
export async function clientFlags(deps: FlagsDeps, req: Request, now: Date, cache?: EdgeCache | null): Promise<Response> {
  if (req.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
  const url = new URL(req.url);
  const fromQuery = url.searchParams.get("key");
  const cacheKey = fromQuery && cache ? new Request(url.toString(), { method: "GET" }) : null;
  if (cacheKey && cache) {
    const hit = await cache.match(cacheKey).catch(() => undefined);
    if (hit) return hit;
  }
  const key = fromQuery ?? req.headers.get("x-flags-key");
  try {
    const ctx = readContext(url.searchParams);
    if (typeof ctx === "string") return reply({ error: ctx }, 400, { ...CORS, "cache-control": "no-store" });
    const body = await evaluate(deps, key, now, ctx);
    // Only a query-keyed URL may sit in a shared cache (the key is the cache key).
    const control = fromQuery ? `public, max-age=${EVAL_MAX_AGE_SECONDS}, s-maxage=${EVAL_MAX_AGE_SECONDS}` : "no-store";
    const res = reply(body, 200, { ...CORS, "cache-control": control });
    if (cacheKey && cache) await cache.put(cacheKey, res.clone()).catch(() => undefined);
    return res;
  } catch (e) {
    if (e instanceof FlagsError) return reply({ error: e.message }, e.status, { ...CORS, "cache-control": "no-store" });
    console.error(`flags client read failed: ${(e as Error).message}`);
    return reply({ error: "Flags could not be read. Try again shortly." }, 500, { ...CORS, "cache-control": "no-store" });
  }
}

const COHORT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/** The person a read is for (QY1.3), from the query; a plain read has none. */
export function readContext(q: URLSearchParams): TargetingContext | string {
  const ctx: TargetingContext = {};
  const id = q.get("id");
  if (id !== null) {
    if (id.length < 1 || id.length > 200) return "id must be 1 to 200 characters";
    ctx.id = id;
  }
  const props = q.get("props");
  if (props !== null) {
    if (props.length > 2048) return "props must be at most 2 KB of JSON";
    let parsed: unknown;
    try {
      parsed = JSON.parse(props);
    } catch {
      return "props must be a JSON object";
    }
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return "props must be a JSON object";
    ctx.properties = parsed as Record<string, unknown>;
  }
  const cohorts = q.get("cohorts");
  if (cohorts !== null) {
    const list = cohorts.split(",").filter(Boolean);
    if (list.length > 50 || !list.every((c) => COHORT_ID.test(c))) return "list at most 50 ids, separated by commas";
    ctx.cohorts = list;
  }
  return ctx;
}

/**
 * POST /api/flags (signed-in console user, same origin): `{app, env, flag}`
 * creates or changes one flag of an environment of the user's own org. The
 * environment is created on first use for an app the org has.
 */
export async function consoleSetFlag(
  deps: FlagsDeps,
  repo: Repo,
  session: { org: string; uid: string } | null,
  body: unknown,
  now: Date,
): Promise<{ status: number; body: Record<string, unknown> }> {
  if (!session) return { status: 401, body: { error: "Sign in again." } };
  const b = (body ?? {}) as Record<string, unknown>;
  if (typeof b.app !== "string" || typeof b.env !== "string") return { status: 400, body: { error: "Name the app and the environment." } };
  try {
    let env = await deps.store.findEnv(session.org, b.app, b.env);
    if (!env) {
      if (!(await repo.findAppByName(session.org, b.app))) return { status: 404, body: { error: "There is no app with that name in your organization." } };
      const org = await repo.getOrg(session.org);
      env = (await ensureEnv(deps, { orgId: session.org, orgName: org?.name ?? "Railyard", app: b.app, env: b.env }, now)).env;
    }
    const flag = await setFlag(deps, env, b.flag, `user:${session.uid}`, now);
    return { status: 200, body: { flag, flags: await listFlags(deps, env) } };
  } catch (e) {
    if (e instanceof FlagsError) return { status: e.status, body: { error: e.message } };
    throw e;
  }
}

/**
 * GET /api/v1/flags?app= with an org's `ry_` token (the agent surface, PY3.4):
 * the org's flag environments and their flags, read only. A token only ever
 * reads its own org.
 */
export async function tokenListFlags(
  deps: FlagsDeps,
  who: { orgId: string } | null,
  app: string | null,
): Promise<{ status: number; body: Record<string, unknown> }> {
  if (!who) return { status: 401, body: { error: "unauthorized" } };
  if (app !== null && !/^[a-z0-9][a-z0-9-]{0,39}$/.test(app)) return { status: 400, body: { error: "app must be an app name (lowercase letters, digits, dashes)" } };
  try {
    const envs = (await deps.store.listEnvs(who.orgId)).filter((e) => app === null || e.app === app).slice(0, 100);
    const environments = [];
    for (const env of envs) environments.push({ app: env.app, env: env.env, flags: await listFlags(deps, env) });
    return { status: 200, body: { environments } };
  } catch (e) {
    if (e instanceof FlagsError) return { status: e.status, body: { error: e.message } };
    throw e;
  }
}

/**
 * POST /api/flags/segments (signed-in console user, same origin):
 * `{app, env, segment}` creates or changes one segment (a named set of
 * property conditions) of an environment of the user's own org;
 * `{app, env, remove: key}` removes one no rule uses (QY1.3).
 */
export async function consoleSegment(
  deps: FlagsDeps,
  session: { org: string; uid: string } | null,
  body: unknown,
  now: Date,
): Promise<{ status: number; body: Record<string, unknown> }> {
  if (!session) return { status: 401, body: { error: "Sign in again." } };
  const b = (body ?? {}) as Record<string, unknown>;
  if (typeof b.app !== "string" || typeof b.env !== "string") return { status: 400, body: { error: "Name the app and the environment." } };
  try {
    const env = await deps.store.findEnv(session.org, b.app, b.env);
    if (!env) return { status: 404, body: { error: "Add a flag to this environment first." } };
    if (typeof b.remove === "string") {
      const removed = await deleteSegment(deps, env, b.remove);
      return { status: removed ? 200 : 404, body: removed ? { segments: await listSegments(deps, env) } : { error: "There is no segment with that key." } };
    }
    const segment = await setSegment(deps, env, b.segment, `user:${session.uid}`, now);
    return { status: 200, body: { segment, segments: await listSegments(deps, env) } };
  } catch (e) {
    if (e instanceof FlagsError) return { status: e.status, body: { error: e.message } };
    throw e;
  }
}
