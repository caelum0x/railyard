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
