// Standard Webhooks verification, replay and forged-metadata cases (RYP-D1 acceptance 3).
import { test } from "node:test";
import assert from "node:assert/strict";

import { signMetadata } from "../lib/dodo.ts";
import { MemoryRepo } from "../lib/repo.ts";
import { sign, verify } from "../lib/standard-webhooks.ts";
import { handleDodoWebhook } from "../lib/webhook.ts";
import { SESSION_SECRET, WEBHOOK_SECRET, signedWebhook } from "./helpers.ts";

const now = new Date("2026-09-29T12:00:00Z");

async function setup(walletFails = false) {
  const repo = new MemoryRepo(() => now);
  const org = await repo.createOrgForUser({ userId: "u1", email: "a@example.com", name: "a" });
  const wallet: unknown[] = [];
  const dodo = {
    async wallet(customerId: string, e: unknown) {
      if (walletFails) throw new Error("provider down");
      wallet.push({ customerId, e });
      return {};
    },
  };
  const deps = { repo, dodo, webhookSecret: WEBHOOK_SECRET, sessionSecret: SESSION_SECRET, brandId: "brand_railyard", now: () => now };
  const metadata = { service: "railyard", org_id: org.id, kind: "card_check", request_id: "r1", sig: await signMetadata(SESSION_SECRET, org.id, "card_check", "r1") };
  return { repo, org, deps, wallet, metadata };
}

test("verify: good signature passes; tampered body, wrong secret and stale timestamp fail", async () => {
  const ts = String(Math.floor(now.getTime() / 1000));
  const sig = await sign(WEBHOOK_SECRET, "msg_a", ts, "{}");
  const nowS = Math.floor(now.getTime() / 1000);
  assert.equal(await verify(WEBHOOK_SECRET, { id: "msg_a", timestamp: ts, signature: sig }, "{}", nowS), true);
  assert.equal(await verify(WEBHOOK_SECRET, { id: "msg_a", timestamp: ts, signature: `v1,bogus ${sig}` }, "{}", nowS), true, "any listed signature may match");
  assert.equal(await verify(WEBHOOK_SECRET, { id: "msg_a", timestamp: ts, signature: sig }, "{ }", nowS), false);
  assert.equal(await verify(WEBHOOK_SECRET, { id: "msg_b", timestamp: ts, signature: sig }, "{}", nowS), false);
  assert.equal(await verify(`whsec_${btoa("other")}`, { id: "msg_a", timestamp: ts, signature: sig }, "{}", nowS), false);
  assert.equal(await verify(WEBHOOK_SECRET, { id: "msg_a", timestamp: ts, signature: sig }, "{}", nowS + 301), false);
  assert.equal(await verify(WEBHOOK_SECRET, { id: null, timestamp: ts, signature: sig }, "{}", nowS), false);
});

test("an unsigned or mis-signed webhook is refused with 401 and changes nothing", async () => {
  const { repo, deps, metadata } = await setup();
  const wh = await signedWebhook("payment.succeeded", { payment_id: "p1", metadata }, now);
  const res = await handleDodoWebhook(deps, { ...wh.headers, signature: "v1,AAAA" }, wh.body);
  assert.equal(res.status, 401);
  assert.equal(repo.events.length, 0);
});

test("a replayed message id is acknowledged and ignored", async () => {
  const { repo, org, deps, wallet, metadata } = await setup();
  const wh = await signedWebhook("payment.succeeded", { payment_id: "p1", customer: { customer_id: "cus_1" }, metadata }, now, "msg_replay");
  assert.equal((await handleDodoWebhook(deps, wh.headers, wh.body)).body.applied, true);
  const again = await handleDodoWebhook(deps, wh.headers, wh.body);
  assert.deepEqual(again, { status: 200, body: { received: true, replayed: true } });
  assert.equal((await repo.getEntitlement(org.id))?.credit_cents, 100, "credited once");
  assert.equal(wallet.length, 1);
});

test("forged metadata (payment link with a made-up org) never grants anything", async () => {
  const { repo, org, deps } = await setup();
  const wh = await signedWebhook("payment.succeeded", { payment_id: "p2", metadata: { service: "railyard", org_id: org.id, kind: "card_check", request_id: "x", sig: "forged" } }, now);
  const res = await handleDodoWebhook(deps, wh.headers, wh.body);
  assert.equal(res.body.applied, false);
  assert.equal(await repo.getEntitlement(org.id), null);
});

test("another brand's event on the shared business is ignored", async () => {
  const { repo, deps, metadata } = await setup();
  const wh = await signedWebhook("payment.succeeded", { payment_id: "p3", brand_id: "brand_menivor", metadata }, now);
  const res = await handleDodoWebhook(deps, wh.headers, wh.body);
  assert.equal(res.body.reason, "other_brand");
  assert.equal(repo.events.length, 0);
});

test("a failed wallet credit releases the message id so the provider's retry applies it", async () => {
  const failing = await setup(true);
  const wh = await signedWebhook("payment.succeeded", { payment_id: "p4", customer: { customer_id: "cus_4" }, metadata: failing.metadata }, now, "msg_retry");
  assert.equal((await handleDodoWebhook(failing.deps, wh.headers, wh.body)).status, 502);
  assert.equal(failing.repo.events.length, 0);
  assert.equal(await failing.repo.getEntitlement(failing.org.id), null);
});

test("malformed bodies are 400", async () => {
  const { deps } = await setup();
  const ts = String(Math.floor(now.getTime() / 1000));
  const body = "not json";
  const res = await handleDodoWebhook(deps, { id: "m", timestamp: ts, signature: await sign(WEBHOOK_SECRET, "m", ts, body) }, body);
  assert.equal(res.status, 400);
});
