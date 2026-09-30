// Insights depth for customers (Wave P PY3.2): profiles, saved cohorts,
// retention by property, breakdowns, CSV and JSON export and delete by user,
// through the console with the org's own `ry_` token. The console names the
// org itself (never the caller) and forwards to the Insights tenant API with
// the platform token, which customers never see. `app` may be the app's name.

import { access } from "../ladder.ts";
import { pausedMessage } from "../apps.ts";
import type { Repo } from "../repo.ts";
import { orgForBearer } from "../tokens.ts";

type Fetch = typeof fetch;

export interface ForwardDeps {
  repo: Repo;
  insightsApi: { url: string; token: string } | null;
  /** RAILYARD_REPLAY_LIVE: off answers 503 with ARRIVING (denylist rule railyard-slot-replay). */
  live: boolean;
  fetchImpl?: Fetch;
}

export const ARRIVING = "This part of Insights is arriving; it is not switched on for this console yet.";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const USER_ID = /^[A-Za-z0-9@._:+=-]{1,50}$/;
const PASS = ["user", "limit", "offset", "period", "periods", "days", "property", "event", "dimension", "kind", "format", "from", "to"];

/** Which calls exist: method and path shape. Anything else is 404. */
export function route(method: string, path: string[]): string | null {
  const [a, b, ...rest] = path;
  if (rest.length) return null;
  if (method === "GET" && b === undefined && a && ["profiles", "retention", "breakdown", "export", "cohorts"].includes(a)) return a;
  if (method === "POST" && a === "cohorts" && b === undefined) return a;
  if ((method === "GET" || method === "DELETE") && a === "cohorts" && b && UUID.test(b)) return `cohorts/${b}`;
  if (method === "DELETE" && a === "users" && b && USER_ID.test(b)) return `users/${encodeURIComponent(b)}`;
  return null;
}

const reply = (body: unknown, status: number) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

export async function forwardInsights(deps: ForwardDeps, req: Request, path: string[], now: Date): Promise<Response> {
  const who = await orgForBearer(deps.repo, req.headers.get("authorization"));
  if (!who) return reply({ error: "unauthorized" }, 401);
  if (!deps.live) return reply({ error: ARRIVING }, 503);
  if (!deps.insightsApi) return reply({ error: "Insights is not connected to this console yet." }, 503);
  const paused = pausedMessage(access(await deps.repo.getEntitlement(who.orgId), now));
  if (paused) return reply({ error: paused }, 402);
  const target = route(req.method, path);
  if (!target) return reply({ error: "not found" }, 404);
  const inUrl = new URL(req.url);
  const q = new URLSearchParams();
  for (const k of PASS) {
    const v = inUrl.searchParams.get(k);
    if (v !== null) q.set(k, v);
  }
  const app = inUrl.searchParams.get("app");
  if (app) {
    const found = UUID.test(app) ? await deps.repo.getApp(who.orgId, app) : await deps.repo.findAppByName(who.orgId, app.toLowerCase());
    if (!found) return reply({ error: `There is no app called ${app} in your organization.` }, 404);
    q.set("app", found.id);
  }
  const url = `${deps.insightsApi.url}/api/v1/tenants/${who.orgId}/${target}${q.size ? `?${q}` : ""}`;
  const body = req.method === "POST" ? await req.text() : undefined;
  if (body !== undefined && body.length > 20_000) return reply({ error: "body too large" }, 413);
  const res = await (deps.fetchImpl ?? fetch)(url, {
    method: req.method,
    headers: { authorization: `Bearer ${deps.insightsApi.token}`, accept: "application/json", ...(body !== undefined ? { "content-type": "application/json" } : {}) },
    body,
  });
  const headers: Record<string, string> = { "cache-control": "no-store", "content-type": res.headers.get("content-type") ?? "application/json" };
  for (const h of ["content-disposition", "x-export-truncated"]) {
    const v = res.headers.get(h);
    if (v) headers[h] = v;
  }
  if (res.status >= 500) return reply({ error: "Insights could not answer; try again in a minute." }, 502);
  return new Response(res.body, { status: res.status, headers });
}
