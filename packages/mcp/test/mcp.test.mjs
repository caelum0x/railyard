// Railyard MCP server (RYP-D5 acceptance 1 to 3) through a real MCP client.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { InMemoryTransport } from "@modelcontextprotocol/sdk/inMemory.js";

import { createServer } from "../src/server.mjs";
import { TOOLS } from "../src/tools.mjs";
import { startConsole } from "./console.mjs";

const TMP = mkdtempSync(join(tmpdir(), "railyard-mcp-"));
let con;
before(async () => {
  con = await startConsole();
});
after(async () => {
  await con.close();
  rmSync(TMP, { recursive: true, force: true });
});

function signIn(name, token) {
  const dir = join(TMP, name);
  mkdirSync(dir, { recursive: true });
  if (token) writeFileSync(join(dir, "credentials.json"), JSON.stringify({ console: con.url, token }));
  return dir;
}

async function client(env, fetchImpl = fetch) {
  const [a, b] = InMemoryTransport.createLinkedPair();
  await createServer({ env, fetchImpl }).connect(a);
  const c = new Client({ name: "test", version: "1" });
  await c.connect(b);
  return c;
}

const call = async (c, name, args = {}) => {
  const r = await c.callTool({ name, arguments: args });
  return { ...r, data: r.structuredContent, text: r.content?.[0]?.text ?? "" };
};

test("every tool declares readOnlyHint or destructiveHint (annotation linter)", async () => {
  const c = await client({ RAILYARD_CONFIG_DIR: signIn("lint", null) });
  const { tools } = await c.listTools();
  assert.deepEqual(tools.map((t) => t.name).sort(), ["deploy.list", "deploy.run", "flags.list", "gateway.traces", "insights.funnel", "insights.summary", "usage.current"]);
  for (const t of tools) {
    const a = t.annotations ?? {};
    assert.ok(typeof a.readOnlyHint === "boolean" || typeof a.destructiveHint === "boolean", `${t.name} has no read-only or destructive hint`);
    assert.ok(!(a.readOnlyHint === true && a.destructiveHint === true), `${t.name}: contradictory hints`);
    assert.ok(t.title && t.description.length > 20, `${t.name} is described`);
  }
  assert.equal(TOOLS.length, tools.length);
});

test("deploy.run returns the exact command, with login first when signed out", async () => {
  const out = await call(await client({ RAILYARD_CONFIG_DIR: signIn("none", null) }), "deploy.run", { app: "shop", branch: "feat/x" });
  assert.equal(out.data.command, "npx railyard deploy --app shop --branch feat/x");
  assert.deepEqual(out.data.steps, ["npx railyard login", "npx railyard deploy --app shop --branch feat/x"]);
  const bad = await call(await client({ RAILYARD_CONFIG_DIR: signIn("none", null) }), "deploy.run", { app: "Shop; rm -rf /" });
  assert.equal(bad.isError, true);
  assert.match(bad.text, /wrong format/);
});

test("a token for org A reads only org A (deploy.list, usage.current)", async () => {
  const ca = await client({ RAILYARD_CONFIG_DIR: signIn("alice", con.a.token) });
  const cb = await client({ RAILYARD_CONFIG_DIR: signIn("bob", con.b.token) });
  const la = await call(ca, "deploy.list");
  const lb = await call(cb, "deploy.list");
  assert.deepEqual(la.data.deploys.map((d) => d.app), ["alice-shop"]);
  assert.deepEqual(lb.data.deploys.map((d) => d.app), ["bob-app"]);
  // Asking org A's token for org B's app name returns nothing, not B's rows.
  assert.deepEqual((await call(ca, "deploy.list", { app: "bob-app" })).data.deploys, []);
  const ua = await call(ca, "usage.current");
  assert.equal(ua.data.org.id, con.a.org.id);
  assert.deepEqual(ua.data.apps.names, ["alice-shop"]);
  assert.equal(ua.data.plan_state, "trial");
  assert.ok(!JSON.stringify([la, ua]).includes(con.a.token), "the token never appears in tool output");
});

test("signed out or expired: a clear error, never an empty success", async () => {
  const none = await call(await client({ RAILYARD_CONFIG_DIR: signIn("none2", null) }), "deploy.list");
  assert.equal(none.isError, true);
  assert.match(none.text, /npx railyard login/);
  const bad = await call(await client({ RAILYARD_CONFIG_DIR: signIn("bad", "ry_notavalidtokenxxxxxxxxxxxx") }), "usage.current");
  assert.equal(bad.isError, true);
  assert.match(bad.text, /expired/);
});

test("insights: honest 'no data yet' without a query store; real numbers when one answers", async () => {
  const dir = signIn("alice2", con.a.token);
  const noStore = await call(await client({ RAILYARD_CONFIG_DIR: dir }), "insights.summary", { app: "alice-shop" });
  assert.equal(noStore.data.status, "no_data_yet");
  const funnel = await call(await client({ RAILYARD_CONFIG_DIR: dir }), "insights.funnel", { app: "alice-shop", steps: ["/", "/signup"] });
  assert.equal(funnel.data.status, "no_data_yet");
  const seen = [];
  const fakeFetch = async (url, init) => {
    seen.push({ url: String(url), auth: init?.headers?.authorization });
    if (String(url).startsWith("https://insights-query.test/")) return new Response(JSON.stringify({ visitors: 42, pageviews: 97 }), { status: 200 });
    return fetch(url, init);
  };
  const live = await call(await client({ RAILYARD_CONFIG_DIR: dir, RAILYARD_INSIGHTS_QUERY_URL: "https://insights-query.test" }, fakeFetch), "insights.summary", { app: "alice-shop", days: 1 });
  assert.deepEqual(live.data, { status: "ok", visitors: 42, pageviews: 97 });
  assert.match(seen[0].url, /\/v1\/query\/summary\?app=alice-shop&days=1$/);
  assert.equal(seen[0].auth, `Bearer ${con.a.token}`);
});

test("flags.list (PY3.4) lists only the signed-in org's flags; gateway.traces reads only the configured key's traces", async () => {
  const dir = signIn("alice3", con.a.token);
  const flags = await call(await client({ RAILYARD_CONFIG_DIR: dir }), "flags.list");
  assert.deepEqual(flags.data.environments, [{ app: "alice-shop", env: "production", flags: [{ key: "new_checkout", enabled: true, value: "v2", description: "The new checkout" }] }]);
  assert.ok(!flags.text.includes("bob_secret_flag"), "org A's token never reads org B's flags");
  const bobs = await call(await client({ RAILYARD_CONFIG_DIR: signIn("bob3", con.b.token) }), "flags.list", { app: "alice-shop" });
  assert.deepEqual(bobs.data.environments, [], "naming org A's app from org B lists nothing");
  const out = await call(await client({ RAILYARD_CONFIG_DIR: signIn("nobody3", null) }), "flags.list");
  assert.equal(out.isError, true);
  const off = await call(await client({ RAILYARD_CONFIG_DIR: dir }), "gateway.traces");
  assert.equal(off.data.status, "not_connected");
  const relayFetch = async (url, init) => {
    assert.equal(init.headers.authorization, "Bearer relay-key-a");
    assert.match(String(url), /^https:\/\/relay\.test\/relay\/traces\?limit=5&window_hours=24$/);
    return new Response(JSON.stringify({ traces: [{ inference_id: "i1", created_at_us: 1790000000000000, function_name: "chat", model_name: "claude-haiku", model_provider_name: "anthropic", input_tokens: 10, output_tokens: 5, cost_micros: 25, processing_time_ms: 400, finish_reason: "stop" }], next_before: null }));
  };
  const on = await call(await client({ RAILYARD_CONFIG_DIR: dir, RAILYARD_GATEWAY_URL: "https://relay.test", RAILYARD_GATEWAY_KEY: "relay-key-a" }, relayFetch), "gateway.traces", { limit: 5 });
  assert.equal(on.data.traces[0].model, "claude-haiku");
  assert.equal(on.data.traces[0].cost_usd, 0.000025);
  assert.ok(!on.text.includes("relay-key-a"));
});
