import { test } from "node:test";
import assert from "node:assert/strict";
import { PostgrestRepo } from "../lib/postgrest-repo.ts";

function fake(status: number, rows: unknown[] = []) {
  const calls: { url: string; init: RequestInit }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return new Response(status === 204 ? null : JSON.stringify(rows), { status });
  }) as typeof fetch;
  return { calls, fetchImpl };
}

test("talks to schema railyard with the service role, parameters encoded", async () => {
  const f = fake(200, [{ id: "o1" }]);
  const repo = new PostgrestRepo({ url: "https://p.scribase.com/", serviceRoleKey: "srk", fetchImpl: f.fetchImpl });
  await repo.getOrg("a&b=c");
  const h = new Headers(f.calls[0].init.headers);
  assert.equal(f.calls[0].url, "https://p.scribase.com/rest/v1/orgs?id=eq.a%26b%3Dc");
  assert.equal(h.get("accept-profile"), "railyard");
  assert.equal(h.get("authorization"), "Bearer srk");
});

test("a duplicate webhook id (409) reads as a replay, not an error", async () => {
  const repo = new PostgrestRepo({ url: "https://p", serviceRoleKey: "k", fetchImpl: fake(409).fetchImpl });
  assert.equal(await repo.recordBillingEvent({ webhook_id: "dodo:x", org_id: null, event_type: "t", payment_id: null, kind: null, amount_cents: null, outcome: "o" }), false);
  const ok = new PostgrestRepo({ url: "https://p", serviceRoleKey: "k", fetchImpl: fake(201).fetchImpl });
  assert.equal(await ok.recordBillingEvent({ webhook_id: "dodo:y", org_id: null, event_type: "t", payment_id: null, kind: null, amount_cents: null, outcome: "o" }), true);
});

test("errors never include the key", async () => {
  const repo = new PostgrestRepo({ url: "https://p", serviceRoleKey: "super-secret-key", fetchImpl: fake(500).fetchImpl });
  await assert.rejects(repo.listApps("o"), (e: Error) => !e.message.includes("super-secret-key") && /failed with 500/.test(e.message));
});

test("Scribase bridge: a replayed bridge message (409) reads as a replay; binding ignores duplicates and reads the winner", async () => {
  const replay = new PostgrestRepo({ url: "https://p", serviceRoleKey: "k", fetchImpl: fake(409).fetchImpl });
  assert.equal(await replay.recordBridgeMessage("scribase:m1"), false);
  const calls: { url: string; init: RequestInit }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    if (init.method === "POST") return new Response(null, { status: 201 });
    return new Response(JSON.stringify([{ scribase_org_id: "org_a", org_id: "winner", created_at: "t" }]), { status: 200 });
  }) as typeof fetch;
  const repo = new PostgrestRepo({ url: "https://p", serviceRoleKey: "k", fetchImpl });
  const b = await repo.bindScribaseOrg("org_a", "loser");
  assert.equal(b.org_id, "winner");
  assert.equal(new Headers(calls[0].init.headers).get("prefer"), "resolution=ignore-duplicates,return=minimal");
  assert.equal(calls[1].url, "https://p/rest/v1/scribase_bindings?scribase_org_id=eq.org_a");
});

test("AI usage goes through the atomic rpc; traces list only the given keys", async () => {
  const calls: { url: string; init: RequestInit }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    calls.push({ url, init });
    return new Response("[]", { status: 200 });
  }) as typeof fetch;
  const repo = new PostgrestRepo({ url: "https://p", serviceRoleKey: "k", fetchImpl });
  await repo.addAiUsage("k1", "2026-09-29", 12.7);
  assert.equal(calls[0].url, "https://p/rest/v1/rpc/ai_add_usage");
  assert.deepEqual(JSON.parse(String(calls[0].init.body)), { p_key: "k1", p_day: "2026-09-29", p_tokens: 12 });
  assert.deepEqual(await repo.listAiTraces([], 10), []);
  await repo.listAiTraces(["a", "b"], 500);
  assert.equal(calls[1].url, "https://p/rest/v1/ai_traces?key_id=in.(a,b)&order=at.desc&limit=200");
});
