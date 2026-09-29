// Railyard Watch (Wave P PY2.2): checks, the agent's list, results with state
// changes and owner email through Rally Send, and the public status view.
//   node --test lib/watch/watch.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";

import { MemoryRepo } from "../repo.ts";
import { emptyEntitlement } from "../types.ts";
import { MemoryWatchStore, PostgrestWatchStore } from "./store.ts";
import { rallySend, type Mailer } from "./mail.ts";
import { agentAuthorized, agentChecks, checkUrl, createCheck, listWithState, nextStatus, recordResults, statusView, watchAllowed, WatchError, type WatchDeps } from "./service.ts";

const T0 = new Date("2026-09-29T12:00:00Z");
const at = (min: number) => new Date(T0.getTime() + min * 60_000).toISOString();

function setup() {
  const repo = new MemoryRepo(() => T0);
  const store = new MemoryWatchStore();
  const sent: { to: string; subject: string; idempotencyKey: string; text: string }[] = [];
  const mailer: Mailer = { send: async (m) => (sent.push(m), true) };
  const deps: WatchDeps = { store, repo, mailer, consoleUrl: "https://railyard.example", agentToken: "agent-token-0123456789", live: true };
  return { repo, store, sent, deps };
}

async function addonOrg(repo: MemoryRepo, slots = ["uptime"], active = true) {
  const org = await repo.createOrgForUser({ userId: crypto.randomUUID(), email: "x@scribase.invalid", name: "Acme" });
  await repo.saveEntitlement({ ...emptyEntitlement(org.id, T0), plan: "addon", status: active ? "active" : "inactive", addon_slots: slots });
  return org.id;
}

test("checkUrl takes public https app addresses only", () => {
  assert.equal(checkUrl("https://shop.example.com/health#x"), "https://shop.example.com/health");
  for (const bad of ["http://shop.example.com", "https://127.0.0.1", "https://localhost", "https://[::1]/", "https://db.internal", "https://u:p@shop.example.com", "https://shop.example.com:5432", "https://relay.ereboslab.com", "nope", 3, "https://intranet"]) {
    assert.ok(checkUrl(bad) instanceof WatchError, String(bad));
  }
});

test("create: one check per app and URL, a status page slug, the per-org cap, only the uptime slot's orgs are checked", async () => {
  const { repo, deps } = setup();
  const org = await addonOrg(repo);
  const c = await createCheck(deps, { orgId: org, app: "shop", url: "https://shop.example.com", ownerEmail: "owner@example.com" });
  assert.match(c.slug, /^shop-[a-z0-9]{6}$/);
  assert.equal((await createCheck(deps, { orgId: org, app: "shop", url: "https://shop.example.com", ownerEmail: null })).id, c.id, "idempotent");
  const other = await addonOrg(repo, ["deploy"]);
  await createCheck(deps, { orgId: other, app: "blog", url: "https://blog.example.com", ownerEmail: null });
  const lapsed = await addonOrg(repo, ["uptime"], false);
  await createCheck(deps, { orgId: lapsed, app: "old", url: "https://old.example.com", ownerEmail: null });
  assert.deepEqual((await agentChecks(deps, T0)).map((x) => x.url), ["https://shop.example.com/"]);
  await assert.rejects(createCheck(deps, { orgId: org, app: "shop", url: "http://x.example.com", ownerEmail: null }), (e: WatchError) => e.status === 400);
  await assert.rejects(createCheck(deps, { orgId: org, app: "shop", name: "<b>", url: "https://y.example.com", ownerEmail: null }), (e: WatchError) => e.status === 400);
  for (let i = 1; i < 20; i++) await createCheck(deps, { orgId: org, app: `a${i}`, url: `https://a${i}.example.com`, ownerEmail: null });
  await assert.rejects(createCheck(deps, { orgId: org, app: "one-more", url: "https://z.example.com", ownerEmail: null }), (e: WatchError) => e.status === 409);
});

test("results: red after two failures, one email per change through Rally Send, back up emails once more", async () => {
  const { repo, deps, sent, store } = setup();
  const org = await addonOrg(repo);
  const c = await createCheck(deps, { orgId: org, app: "shop", url: "https://shop.example.com", ownerEmail: "owner@example.com" });
  await recordResults(deps, { results: [{ id: c.id, ok: true, at: at(0), ms: 120 }] }, new Date(at(0)));
  assert.equal(sent.length, 0, "first up is not news");
  await recordResults(deps, { results: [{ id: c.id, ok: false, at: at(1), ms: 10_000 }] }, new Date(at(1)));
  assert.equal((await store.getStatus(c.id))?.state, "up", "one failure is not down");
  await recordResults(deps, { results: [{ id: c.id, ok: false, at: at(2), ms: 10_000 }] }, new Date(at(2)));
  assert.equal((await store.getStatus(c.id))?.state, "down");
  assert.equal(sent.length, 1);
  assert.equal(sent[0].to, "owner@example.com");
  assert.equal(sent[0].subject, "shop is down");
  assert.match(sent[0].text, /https:\/\/railyard\.example\/status\/shop-/);
  assert.doesNotMatch(sent[0].text, /shop\.example\.com/, "the email names no host");
  await recordResults(deps, { results: [{ id: c.id, ok: false, at: at(3), ms: 10_000 }] }, new Date(at(3)));
  assert.equal(sent.length, 1, "still down: no second email");
  await recordResults(deps, { results: [{ id: c.id, ok: true, at: at(4), ms: 90 }, { id: c.id, ok: true, at: at(3.5), ms: 90 }] }, new Date(at(4)));
  assert.equal(sent.length, 2);
  assert.equal(sent[1].subject, "shop is back up");
  // Red within 5 minutes of the app stopping: results every minute, two failures.
  const status = (await store.getStatus(c.id))!;
  assert.equal(status.state, "up");
  assert.equal(status.first_result_at, at(0));
});

test("results: a failed email is retried on the next result with the same idempotency key; stale and foreign results are ignored", async () => {
  const { repo, deps, store } = setup();
  const org = await addonOrg(repo);
  const c = await createCheck(deps, { orgId: org, app: "shop", url: "https://shop.example.com", ownerEmail: "owner@example.com" });
  const keys: string[] = [];
  let ok = false;
  deps.mailer = { send: async (m) => (keys.push(m.idempotencyKey), ok) };
  await recordResults(deps, { results: [{ id: c.id, ok: false, at: at(0), ms: 1 }, { id: c.id, ok: false, at: at(1), ms: 1 }] }, new Date(at(1)));
  ok = true;
  await recordResults(deps, { results: [{ id: c.id, ok: false, at: at(2), ms: 1 }] }, new Date(at(2)));
  assert.equal(keys.length, 2);
  assert.equal(keys[0], keys[1]);
  assert.equal((await store.getStatus(c.id))?.alerted_state, "down");
  const r = await recordResults(deps, { results: [{ id: c.id, ok: true, at: at(1.5), ms: 1 }, { id: crypto.randomUUID(), ok: true, at: at(3), ms: 1 }, { id: c.id, ok: true, at: at(90), ms: 1 }, { nope: 1 }] }, new Date(at(3)));
  assert.equal(r.accepted, 0, "older, unknown, future and malformed results change nothing");
  await assert.rejects(recordResults(deps, { results: "x" }, T0), (e: WatchError) => e.status === 400);
});

test("status view: live results only, the name and never the address", async () => {
  const { repo, deps, store } = setup();
  const org = await addonOrg(repo);
  const c = await createCheck(deps, { orgId: org, app: "shop", name: "Shop", url: "https://shop.example.com", ownerEmail: null });
  assert.equal(statusView(c, null).label, "Waiting for the first check");
  await recordResults(deps, { results: Array.from({ length: 10 }, (_, i) => ({ id: c.id, ok: i !== 3, at: at(i), ms: 100 })) }, new Date(at(10)));
  const v = statusView(c, await store.getStatus(c.id));
  assert.equal(v.label, "Operational");
  assert.equal(v.uptime, "90.0% of the last 10 checks");
  assert.equal(v.bars.length, 10);
  assert.doesNotMatch(JSON.stringify(v), /example\.com/);
  const listed = await listWithState(deps, org);
  assert.equal(listed[0].state, "up");
  assert.match(listed[0].status_url, /\/status\/shop-/);
});

test("the agent needs its bearer token; nextStatus keeps 90 points", () => {
  const deps = { agentToken: "agent-token-0123456789", live: true };
  assert.equal(agentAuthorized(deps, "Bearer agent-token-0123456789"), true);
  assert.equal(agentAuthorized(deps, "Bearer wrong"), false);
  assert.equal(agentAuthorized({ agentToken: null }, "Bearer x"), false);
  let s = { check_id: "c", state: "waiting" as const, consecutive_failures: 0, first_result_at: null, last_checked_at: null, last_change_at: null, alerted_state: null, history: [] };
  let cur: ReturnType<typeof nextStatus> = s;
  for (let i = 0; i < 120; i++) cur = nextStatus(cur, { id: "c", ok: true, at: at(i), ms: 1 });
  assert.equal(cur.history.length, 90);
  void s;
});

test("watchAllowed: a Railyard plan or the add-on with uptime", () => {
  assert.equal(watchAllowed(null, T0), false);
  assert.equal(watchAllowed({ ...emptyEntitlement("o", T0), plan: "railyard", status: "active" }, T0), true);
  assert.equal(watchAllowed({ ...emptyEntitlement("o", T0), plan: "addon", status: "active", addon_slots: ["deploy"] }, T0), false);
});

test("Rally Send: product railyard, from status, the key only in the header", async () => {
  const calls: { url: string; init: RequestInit }[] = [];
  const m = rallySend({ url: "https://vcpeer.example/api/rally/v1/transactional/send", key: "tx-key", fetchImpl: (async (url: string, init: RequestInit) => (calls.push({ url, init }), new Response("{}", { status: 202 }))) as typeof fetch });
  assert.equal(await m.send({ to: "a@example.com", subject: "s", text: "t", html: "<p>t</p>", idempotencyKey: "watch:1:down:x", project: "org1" }), true);
  const body = JSON.parse(String(calls[0].init.body));
  assert.equal(body.product, "railyard");
  assert.equal(body.from_local, "status");
  assert.equal(new Headers(calls[0].init.headers).get("authorization"), "Bearer tx-key");
  assert.ok(!String(calls[0].init.body).includes("tx-key"));
});

test("the PostgREST store: schema railyard; pausing is scoped to the org", async () => {
  const urls: string[] = [];
  const store = new PostgrestWatchStore({ url: "https://p", serviceRoleKey: "k", fetchImpl: (async (url: string) => (urls.push(url), new Response("[]", { status: 200 }))) as typeof fetch });
  assert.equal(await store.setPaused("org1", "c1", true), null);
  assert.equal(urls[0], "https://p/rest/v1/watch_checks?id=eq.c1&org_id=eq.org1");
  await store.activeChecks();
  assert.match(urls[1], /watch_checks\?paused=is\.false/);
});

test("RAILYARD_WATCH_LIVE off: no check is made, the agent gets an empty list, results are refused", async () => {
  const { repo, deps } = setup();
  const org = await addonOrg(repo);
  const c = await createCheck(deps, { orgId: org, app: "shop", url: "https://shop.example.com", ownerEmail: null });
  const off: WatchDeps = { ...deps, live: false };
  await assert.rejects(createCheck(off, { orgId: org, app: "blog", url: "https://blog.example.com", ownerEmail: null }), (e: WatchError) => e.status === 503 && /arriving/.test(e.message));
  assert.deepEqual(await agentChecks(off, T0), []);
  await assert.rejects(recordResults(off, { results: [{ id: c.id, ok: true, at: at(0), ms: 1 }] }, T0), (e: WatchError) => e.status === 503);
});
