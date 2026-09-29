// HTTP handlers for Railyard Flags (Wave P PY2.1), framework-free so the
// route files stay three lines and the tests call them directly.

import type { Repo } from "../repo.ts";
import { ensureEnv, evaluate, EVAL_MAX_AGE_SECONDS, FlagsError, listFlags, setFlag, type FlagsDeps } from "./service.ts";

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
 * environment's flags for an app. Public, CORS open. A URL-keyed answer is
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
    const body = await evaluate(deps, key, now);
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
