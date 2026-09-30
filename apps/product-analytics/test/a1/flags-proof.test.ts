// /flags/?v=2 and /decide/ answer plain on/off plus value, read through the
// flags slot's public client API (a local stand-in with the slot's exact
// response shape); variants.ts is the A3 skeleton (null). The bridge proof
// route answers first_product_event under Standard Webhooks.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { randomUUID } from "node:crypto";
import { sign } from "../../src/bridge/standard-webhooks.ts";
import { HttpFlagsReader } from "../../src/flags/client.ts";
import { variantFor } from "../../src/flags/variants.ts";
import { signConsoleToken } from "../../src/tenancy/console-token.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

const SECRET = "f".repeat(40);
const BRIDGE = `whsec_${Buffer.from("bridge-secret-for-tests-0123456789").toString("base64")}`;
const KEY = "ry_flags_abcdefghijklmnopqrstuvwxyz012345";
let pg: ScratchPg;
let run: Running;
const site = makeSite({ scribase_org_id: "org_flags1" });
const other = makeSite();
const slotHits: string[] = [];

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  // Stand-in for GET /api/v1/flags/client (wave-p2-railyard apps/console/lib/flags/http.ts shape).
  const slot = createServer((req, res) => {
    slotHits.push(req.url ?? "");
    const url = new URL(req.url ?? "/", "http://x");
    if (url.pathname !== "/api/v1/flags/client" || url.searchParams.get("key") !== KEY) {
      res.writeHead(401, { "content-type": "application/json" }).end(JSON.stringify({ error: "A valid flags key is required." }));
      return;
    }
    res.writeHead(200, { "content-type": "application/json" }).end(
      JSON.stringify({ env: "production", flags: { new_checkout: { enabled: true, value: '{"color":"green"}' }, beta: { enabled: false, value: null }, plain: { enabled: true, value: null } }, fetched_at: new Date().toISOString() }),
    );
  });
  await new Promise<void>((r) => slot.listen(0, "127.0.0.1", () => r()));
  slot.unref();
  const flagsUrl = `http://127.0.0.1:${(slot.address() as AddressInfo).port}`;
  run = await startService(pg, [site, other], { consoleSecret: SECRET, bridgeSecret: BRIDGE, flags: new HttpFlagsReader({ url: flagsUrl }) });
  const tok = await signConsoleToken(SECRET, { org_id: site.org_id, app_ids: [site.project_id], role: "owner" }, new Date());
  const put = await fetch(`${run.base}/v1/projects/${site.project_id}/settings`, { method: "PUT", headers: { authorization: `Bearer ${tok}` }, body: JSON.stringify({ flags_client_key: KEY }) });
  assert.equal(put.status, 200);
  const bad = await fetch(`${run.base}/v1/projects/${site.project_id}/settings`, { method: "PUT", headers: { authorization: `Bearer ${tok}` }, body: JSON.stringify({ flags_client_key: "sk_live_nope" }) });
  assert.equal(bad.status, 400);
});

test("/flags/?v=2 answers each flag with enabled, variant and payload", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/flags/?v=2`, { method: "POST", body: JSON.stringify({ token: site.token, distinct_id: "u1" }) });
  assert.equal(res.status, 200);
  const body = (await res.json()) as { flags: Record<string, { enabled: boolean; variant: string | null; metadata: { payload: string | null } }>; errorsWhileComputingFlags: boolean };
  assert.equal(body.errorsWhileComputingFlags, false);
  assert.deepEqual(Object.keys(body.flags).sort(), ["beta", "new_checkout", "plain"]);
  assert.equal(body.flags.new_checkout.enabled, true);
  assert.equal(body.flags.new_checkout.variant, null);
  assert.equal(body.flags.new_checkout.metadata.payload, '{"color":"green"}');
  assert.equal(body.flags.beta.enabled, false);
  assert.ok(slotHits.every((u) => u.startsWith("/api/v1/flags/client?key=")), "only the public client API is read");
});

test("/decide/ answers the older shape too", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/decide/?v=3`, { method: "POST", body: JSON.stringify({ api_key: site.token, distinct_id: "u1" }) });
  const body = (await res.json()) as { featureFlags: Record<string, unknown>; featureFlagPayloads: Record<string, unknown> };
  assert.deepEqual(body.featureFlags, { new_checkout: true, plain: true });
  assert.deepEqual(body.featureFlagPayloads, { new_checkout: '{"color":"green"}' });
});

test("a project without a flags key has no flags; an unknown token is 401", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/flags/?v=2`, { method: "POST", body: JSON.stringify({ token: other.token, distinct_id: "u1" }) });
  assert.deepEqual(((await res.json()) as { flags: object }).flags, {});
  assert.equal((await fetch(`${run.base}/flags/?v=2`, { method: "POST", body: JSON.stringify({ token: "site_nope00000000", distinct_id: "u" }) })).status, 401);
});

test("variants are A3's: the skeleton answers null", () => {
  assert.equal(variantFor("x", { enabled: true, value: "{}" }, "u"), null);
});

test("bridge proof: signed request -> first_product_event; unsigned or stale -> 401", { skip: skipNoPg }, async () => {
  const call = async (body: object, opts: { secret?: string; at?: number } = {}) => {
    const text = JSON.stringify(body);
    const id = `msg_${randomUUID()}`;
    const ts = String(opts.at ?? Math.floor(Date.now() / 1000));
    return fetch(`${run.base}/v1/bridge/scribase/proof`, {
      method: "POST",
      headers: { "webhook-id": id, "webhook-timestamp": ts, "webhook-signature": sign(opts.secret ?? BRIDGE, id, ts, text), "content-type": "application/json" },
      body: text,
    });
  };
  const waiting = (await (await call({ slot: "analytics", scribase_org_id: "org_flags1" })).json()) as Record<string, unknown>;
  assert.deepEqual(waiting, { slot: "analytics", state: "no_proof_yet", live: true, proof: null, next: "Waiting for the first event from the app." });
  await fetch(`${run.base}/e/`, { method: "POST", body: JSON.stringify({ event: "$pageview", uuid: randomUUID(), properties: { token: site.token, distinct_id: "first" } }) });
  await run.svc.deps.writer.flush();
  const live = (await (await call({ slot: "analytics", org_id: site.org_id })).json()) as { state: string; proof: { kind: string; id: string; at: string } };
  assert.equal(live.state, "live");
  assert.equal(live.proof.kind, "first_product_event");
  assert.equal(live.proof.id, site.project_id);
  assert.ok(Date.parse(live.proof.at) > 0);
  assert.equal((await call({ slot: "analytics", org_id: site.org_id }, { secret: `whsec_${Buffer.from("wrong").toString("base64")}` })).status, 401);
  assert.equal((await call({ slot: "analytics", org_id: site.org_id }, { at: Math.floor(Date.now() / 1000) - 3600 })).status, 401);
  assert.equal((await call({ slot: "flags", org_id: site.org_id })).status, 400);
  const none = (await (await call({ slot: "analytics", org_id: randomUUID() })).json()) as { state: string };
  assert.equal(none.state, "not_entitled");
});
