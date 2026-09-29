// The AI gateway (Wave P PY1.4): BYOK only, caps, traces, no path to our keys.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { MemoryRepo } from "../repo.ts";
import { emptyEntitlement } from "../types.ts";
import { chatCompletion, credentialsFor, NOT_CONNECTED, parseModel, PROVIDER_SLOTS, PROVIDERS } from "./gateway.ts";
import { connectCredential, issueAiKey, issueConnectToken } from "./provision.ts";
import { open } from "./seal.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const MASTER = Buffer.alloc(32, 7).toString("base64");
const NOW = new Date("2026-09-29T12:00:00Z");
const CUSTOMER_KEY = "sk-ant-customer-own-key-0123456789";

async function setup(opts: { connect?: boolean; active?: boolean } = {}) {
  const repo = new MemoryRepo(() => NOW);
  const org = await repo.createOrgForUser({ userId: "u", email: "a@x.com", name: "a" });
  await repo.saveEntitlement({ ...emptyEntitlement(org.id, NOW), plan: "addon", status: opts.active === false ? "inactive" : "active", apps_limit: 5, events_limit: 10 });
  const { token } = await issueAiKey(repo, org.id, "shop-prod", { daily: 1000, monthly: 5000 });
  if (opts.connect !== false) {
    const ct = await issueConnectToken(repo, org.id, NOW);
    const r = await connectCredential({ repo, credentialsKey: MASTER, now: () => NOW }, ct, "anthropic", CUSTOMER_KEY);
    assert.equal(r.ok, true);
  }
  const relayCalls: { body: Record<string, unknown>; auth: string | null }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    relayCalls.push({ body: JSON.parse(String(init.body)), auth: new Headers(init.headers).get("authorization") });
    return new Response(JSON.stringify({ id: "c1", choices: [{ message: { role: "assistant", content: "hi" } }], usage: { prompt_tokens: 12, completion_tokens: 8 } }), { status: 200 });
  }) as typeof fetch;
  const deps = { repo, relayUrl: "https://relay.internal", relayKey: "relay-tenant-key-0123456789", credentialsKey: MASTER, consoleUrl: "https://railyard.example", fetchImpl, now: () => NOW };
  return { repo, org, token, deps, relayCalls };
}
const call = (deps: Parameters<typeof chatCompletion>[0], token: string, body: unknown) => chatCompletion(deps, `Bearer ${token}`, JSON.stringify(body));

test("the customer's own key goes only to its provider's slot; every other slot refuses, so ours are never used", async () => {
  const { deps, token, relayCalls, repo, org } = await setup();
  const r = await call(deps, token, { model: "anthropic/claude-sonnet-4-5", messages: [{ role: "user", content: "hi" }] });
  assert.equal(r.status, 200);
  const sent = relayCalls[0].body;
  assert.equal(sent.model, "tensorzero::model_name::anthropic::claude-sonnet-4-5");
  const creds = sent["tensorzero::credentials"] as Record<string, string>;
  assert.deepEqual(Object.keys(creds).sort(), Object.values(PROVIDER_SLOTS).sort());
  assert.equal(creds.relay_anthropic_api_key, CUSTOMER_KEY);
  for (const [slot, v] of Object.entries(creds)) if (slot !== "relay_anthropic_api_key") assert.equal(v, NOT_CONNECTED, slot);
  assert.equal(relayCalls[0].auth, "Bearer relay-tenant-key-0123456789");
  assert.ok(!JSON.stringify(r.body).includes(CUSTOMER_KEY));
  const traces = await repo.listAiTraces((await repo.listAiKeys(org.id)).map((k) => k.id), 10);
  assert.deepEqual([traces[0].provider, traces[0].model, traces[0].input_tokens, traces[0].output_tokens, traces[0].status], ["anthropic", "claude-sonnet-4-5", 12, 8, 200]);
});

test("BYOK only: no customer credential for the provider means 403 and no call at all", async () => {
  const { deps, token, relayCalls } = await setup({ connect: false });
  const r = await call(deps, token, { model: "anthropic/claude-sonnet-4-5", messages: [] });
  assert.equal(r.status, 403);
  assert.equal((r.body as { error: { code: string } }).error.code, "byok_required");
  const other = await setup();
  const r2 = await call(other.deps, other.token, { model: "openai/gpt-4o-mini", messages: [] });
  assert.equal(r2.status, 403, "an anthropic key does not open openai");
  assert.equal(relayCalls.length + other.relayCalls.length, 0);
});

test("refusals: credentials in the body, streaming, a model without a provider, a bad key, a lapsed add-on", async () => {
  const { deps, token } = await setup();
  assert.equal((await call(deps, token, { model: "anthropic/x", "tensorzero::credentials": { relay_anthropic_api_key: "mine" } })).status, 400);
  assert.equal((await call(deps, token, { model: "anthropic/x", stream: true })).status, 400);
  assert.equal((await call(deps, token, { model: "claude-sonnet-4-5" })).status, 400);
  assert.equal((await call(deps, token, { model: "evil/x" })).status, 400);
  assert.equal((await chatCompletion(deps, "Bearer ry_ai_not-a-real-key-0000000000", "{}")).status, 401);
  assert.equal((await chatCompletion(deps, null, "{}")).status, 401);
  const off = await setup({ active: false });
  assert.equal((await call(off.deps, off.token, { model: "anthropic/x" })).status, 402);
});

test("daily and monthly token caps per key answer 429 with the period", async () => {
  const { deps, token, repo, org } = await setup();
  const key = (await repo.listAiKeys(org.id))[0];
  await repo.addAiUsage(key.id, "2026-09-29", 1000);
  const day = await call(deps, token, { model: "anthropic/x", messages: [] });
  assert.equal(day.status, 429);
  assert.equal((day.body as { error: { period: string } }).error.period, "day");
  const month = await setup();
  const k2 = (await month.repo.listAiKeys(month.org.id))[0];
  await month.repo.addAiUsage(k2.id, "2026-09-02", 5000);
  const m = await call(month.deps, month.token, { model: "anthropic/x", messages: [] });
  assert.equal((m.body as { error: { period: string } }).error.period, "month");
});

test("the connect link works once, expires, and the key is sealed and bound to its org and provider", async () => {
  const { repo, org } = await setup({ connect: false });
  const ct = await issueConnectToken(repo, org.id, NOW);
  const deps = { repo, credentialsKey: MASTER, now: () => NOW };
  assert.equal((await connectCredential(deps, ct, "anthropic", "short")).ok, false);
  const ok = await connectCredential(deps, ct, "anthropic", CUSTOMER_KEY);
  assert.deepEqual(ok, { ok: true, provider: "anthropic", hint: "...6789" });
  const again = await connectCredential(deps, ct, "anthropic", CUSTOMER_KEY);
  assert.equal(again.ok ? 0 : again.status, 410);
  const stored = await repo.getAiCredential(org.id, "anthropic");
  assert.ok(stored && !JSON.stringify(stored).includes(CUSTOMER_KEY));
  assert.equal(await open(MASTER, `${org.id}:anthropic`, stored!.nonce, stored!.ciphertext), CUSTOMER_KEY);
  await assert.rejects(open(MASTER, `${org.id}:openai`, stored!.nonce, stored!.ciphertext));
  const late = await issueConnectToken(repo, org.id, NOW);
  const expired = await connectCredential({ ...deps, now: () => new Date(NOW.getTime() + 16 * 60_000) }, late, "openai", CUSTOMER_KEY);
  assert.equal(expired.ok ? 0 : expired.status, 410);
});

test("the slot list matches Relay's provider key slots (apps/relay/crates/gateway/src/provider_keys.rs)", () => {
  const rust = readFileSync(join(HERE, "../../../relay/crates/gateway/src/provider_keys.rs"), "utf8");
  const block = rust.slice(rust.indexOf("pub const PROVIDER_KEY_SLOTS"), rust.indexOf("];", rust.indexOf("pub const PROVIDER_KEY_SLOTS")));
  const names = [...block.matchAll(/name: "([a-z_]+)"/g)].map((m) => m[1]).sort();
  assert.deepEqual(names, Object.values(PROVIDER_SLOTS).sort(), "a new Relay slot must get a refusal marker here");
  assert.deepEqual(parseModel("tensorzero::model_name::groq::llama-3.3"), { provider: "groq", id: "llama-3.3" });
  assert.equal(Object.keys(credentialsFor("openai", "k")).length, PROVIDERS.length);
});
