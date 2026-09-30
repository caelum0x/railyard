// Insights depth through the console (PY3.2): the org comes from the token,
// the platform token never leaves, app names resolve to the org's own app,
// and only the listed calls pass.
//   node --test lib/insights-depth/forward.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";

import { MemoryRepo } from "../repo.ts";
import { emptyEntitlement } from "../types.ts";
import { sha256Hex } from "../crypto.ts";
import { forwardInsights, route } from "./forward.ts";

const NOW = new Date("2026-09-30T12:00:00Z");
const PLATFORM = "insights-platform-token-0123456789";

async function setup(active = true) {
  const repo = new MemoryRepo(() => NOW);
  const org = await repo.createOrgForUser({ userId: "u1", email: "a@example.com", name: "Acme" });
  await repo.saveEntitlement({ ...emptyEntitlement(org.id, NOW), plan: "addon", status: active ? "active" : "inactive", addon_slots: ["analytics"], apps_limit: 5 });
  const app = await repo.createApp({ orgId: org.id, name: "shop", siteKey: "ry_site_abcdefgh1234" });
  const other = await repo.createOrgForUser({ userId: "u2", email: "b@example.com", name: "Other" });
  const otherApp = await repo.createApp({ orgId: other.id, name: "blog", siteKey: "ry_site_zzzzzzzz1234" });
  const token = `ry_${"a".repeat(30)}`;
  await repo.createToken({ org_id: org.id, user_id: "u1", token_hash: await sha256Hex(token), label: "cli" });
  const calls: { url: string; method: string; auth: string | null; body: string | null }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    calls.push({ url, method: init.method ?? "GET", auth: new Headers(init.headers).get("authorization"), body: (init.body as string) ?? null });
    if (url.includes("/export")) return new Response("time,app_id\r\n", { headers: { "content-type": "text/csv; charset=utf-8", "content-disposition": 'attachment; filename="events.csv"', "x-export-truncated": "false" } });
    if (url.includes("/boom")) return new Response("x", { status: 500 });
    return new Response(JSON.stringify({ ok: true }), { headers: { "content-type": "application/json" } });
  }) as unknown as typeof fetch;
  return { repo, org: org.id, app, otherApp, token, calls, deps: { repo, insightsApi: { url: "https://insights.internal", token: PLATFORM }, live: true, fetchImpl } };
}

const req = (path: string, token: string | null, init: RequestInit = {}) =>
  new Request(`https://railyard.example/api/v1/insights/${path}`, { ...init, headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(init.headers ?? {}) } });

test("the org comes from the token; the platform token is used downstream and never returned", async () => {
  const s = await setup();
  const res = await forwardInsights(s.deps, req("retention?property=trait.plan&period=week&org=someone-else&app=shop", s.token), ["retention"], NOW);
  assert.equal(res.status, 200);
  const c = s.calls[0]!;
  assert.equal(c.url, `https://insights.internal/api/v1/tenants/${s.org}/retention?period=week&property=trait.plan&app=${s.app.id}`);
  assert.equal(c.auth, `Bearer ${PLATFORM}`);
  assert.ok(!(await res.text()).includes(PLATFORM));
});

test("export keeps the CSV type and file name; cohorts are saved by POST; delete by user id passes the id safely", async () => {
  const s = await setup();
  const csv = await forwardInsights(s.deps, req("export?kind=events&format=csv", s.token), ["export"], NOW);
  assert.equal(csv.headers.get("content-type"), "text/csv; charset=utf-8");
  assert.equal(csv.headers.get("content-disposition"), 'attachment; filename="events.csv"');
  assert.equal(await csv.text(), "time,app_id\r\n");
  const body = JSON.stringify({ name: "Active", definition: { performed: { event: "x", within_days: 7 } } });
  await forwardInsights(s.deps, req("cohorts", s.token, { method: "POST", body }), ["cohorts"], NOW);
  assert.equal(s.calls.at(-1)!.method, "POST");
  assert.equal(s.calls.at(-1)!.body, body);
  await forwardInsights(s.deps, req("users/user%4042", s.token, { method: "DELETE" }), ["users", "user@42"], NOW);
  assert.equal(s.calls.at(-1)!.url, `https://insights.internal/api/v1/tenants/${s.org}/users/user%4042`);
});

test("refused: no token, a paused org, another org's app, unknown calls, bad user ids; errors never leak upstream text", async () => {
  const s = await setup();
  assert.equal((await forwardInsights(s.deps, req("profiles", null), ["profiles"], NOW)).status, 401);
  const other = await forwardInsights(s.deps, req(`profiles?app=${s.otherApp.id}`, s.token), ["profiles"], NOW);
  assert.equal(other.status, 404);
  assert.equal((await forwardInsights(s.deps, req("profiles?app=blog", s.token), ["profiles"], NOW)).status, 404);
  assert.equal(s.calls.length, 0, "nothing was forwarded");
  assert.equal(route("GET", ["summary"]), null);
  assert.equal(route("DELETE", ["profiles"]), null);
  assert.equal(route("DELETE", ["users", "a b"]), null);
  assert.equal(route("GET", ["cohorts", "not-a-uuid"]), null);
  assert.equal(route("GET", ["cohorts", "x", "y"]), null);
  assert.equal((await forwardInsights({ ...s.deps, insightsApi: null }, req("profiles", s.token), ["profiles"], NOW)).status, 503);
  const off = await forwardInsights({ ...s.deps, live: false }, req("profiles", s.token), ["profiles"], NOW);
  assert.equal(off.status, 503);
  assert.match((await off.json()).error, /arriving/);
  const paused = await setup(false);
  const p = await forwardInsights(paused.deps, req("profiles", paused.token), ["profiles"], NOW);
  assert.equal(p.status, 402);
  assert.match((await p.json()).error, /renew the add-on/);
});
