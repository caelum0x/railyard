// HTTP handlers for Railyard Watch (Wave P PY2.2), framework-free.

import type { Repo } from "../repo.ts";
import { agentAuthorized, agentChecks, ARRIVING, createCheck, listWithState, recordResults, WatchError, type WatchDeps } from "./service.ts";

const reply = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

/** GET /api/v1/watch/agent/checks: the box 1 agent's list (bearer WATCH_AGENT_TOKEN). */
export async function agentList(deps: WatchDeps, req: Request, now: Date): Promise<Response> {
  if (!deps.agentToken) return reply({ error: "watch_agent_not_configured" }, 503);
  if (!agentAuthorized(deps, req.headers.get("authorization"))) return reply({ error: "unauthorized" }, 401);
  return reply({ checks: await agentChecks(deps, now) });
}

/** POST /api/v1/watch/agent/results: `{results:[{id, ok, at, ms}]}` from the agent. */
export async function agentResults(deps: WatchDeps, req: Request, now: Date): Promise<Response> {
  if (!deps.agentToken) return reply({ error: "watch_agent_not_configured" }, 503);
  if (!agentAuthorized(deps, req.headers.get("authorization"))) return reply({ error: "unauthorized" }, 401);
  const body = await req.json().catch(() => null);
  try {
    return reply(await recordResults(deps, body, now));
  } catch (e) {
    if (e instanceof WatchError) return reply({ error: e.message }, e.status);
    throw e;
  }
}

/**
 * POST /api/watch (signed-in console user, same origin):
 * `{app, url, name?}` adds a check for one of the org's apps;
 * `{id, paused}` pauses or resumes one of the org's checks.
 */
export async function consoleWatch(
  deps: WatchDeps,
  repo: Repo,
  session: { org: string; email: string } | null,
  body: unknown,
): Promise<{ status: number; body: Record<string, unknown> }> {
  if (!session) return { status: 401, body: { error: "Sign in again." } };
  if (!deps.live) return { status: 503, body: { error: ARRIVING } };
  const b = (body ?? {}) as Record<string, unknown>;
  try {
    if (typeof b.id === "string" && typeof b.paused === "boolean") {
      const c = await deps.store.setPaused(session.org, b.id, b.paused);
      return c ? { status: 200, body: { check: { id: c.id, paused: c.paused } } } : { status: 404, body: { error: "There is no such check in your organization." } };
    }
    if (typeof b.app !== "string" || !(await repo.findAppByName(session.org, b.app))) return { status: 404, body: { error: "Pick one of your apps." } };
    const email = /@/.test(session.email) && !session.email.endsWith(".invalid") ? session.email : null;
    const c = await createCheck(deps, { orgId: session.org, app: b.app, name: b.name, url: b.url, ownerEmail: email });
    return { status: 200, body: { checks: await listWithState(deps, session.org, c.app) } };
  } catch (e) {
    if (e instanceof WatchError) return { status: e.status, body: { error: e.message } };
    throw e;
  }
}
