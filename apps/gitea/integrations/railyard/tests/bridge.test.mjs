// Unit tests for the Rithub (Gitea) -> railyard billing bridge's pure
// functions. No network, no Docker — run with: node --test tests/bridge.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import {
  verify,
  verifyRago,
  stableTxnId,
  toRagoEvents,
  isSettleableInvoice,
  resolveOrgAccount,
} from "../webhook-bridge/bridge.mjs";

test("verify: accepts a correct HMAC and rejects a wrong one", () => {
  const secret = "s3cr3t";
  const body = JSON.stringify({ a: 1 });
  const mac = crypto.createHmac("sha256", secret).update(body).digest("hex");
  assert.equal(verify(body, mac, secret), true);
  assert.equal(verify(body, "deadbeef", secret), false);
  assert.equal(verify(body, undefined, secret), false);
});

test("verify: an unset secret accepts anything (dev only)", () => {
  assert.equal(verify("anything", undefined, ""), true);
});

test("verifyRago: fails closed with no secret even if a signature is present", () => {
  assert.equal(verifyRago("body", "somesig", ""), false);
});

test("verifyRago: accepts a correct base64 HMAC", () => {
  const secret = "rago-hmac";
  const body = JSON.stringify({ invoice: { total: 100 } });
  const mac = crypto.createHmac("sha256", secret).update(body).digest("base64");
  assert.equal(verifyRago(body, mac, secret), true);
  assert.equal(verifyRago(body, "bad", secret), false);
});

test("stableTxnId: same delivery id + code always produces the same id (redelivery-safe)", () => {
  const id1 = stableTxnId("delivery-123", "rithub_private_repo", "repository", {});
  const id2 = stableTxnId("delivery-123", "rithub_private_repo", "repository", {});
  assert.equal(id1, id2);
  assert.equal(id1, "rithub_delivery-123_rithub_private_repo");
});

test("stableTxnId: falls back to a deterministic hash when no delivery id is given", () => {
  const payload = { repository: { full_name: "acme/widgets", id: 7 }, after: "abc123" };
  const a = stableTxnId("", "rithub_ci_push", "push", payload);
  const b = stableTxnId("", "rithub_ci_push", "push", payload);
  assert.equal(a, b);
  assert.match(a, /^rithub_[0-9a-f]{40}$/);
  // Different push (different `after`) must hash to a different id.
  const c = stableTxnId("", "rithub_ci_push", "push", { ...payload, after: "xyz999" });
  assert.notEqual(a, c);
});

test("toRagoEvents: a new private repo bills +1 rithub_private_repo", () => {
  const payload = { action: "created", repository: { full_name: "acme/widgets", private: true, owner: { username: "acme" } } };
  const events = toRagoEvents("repository", payload, "d1");
  assert.equal(events.length, 1);
  assert.equal(events[0].code, "rithub_private_repo");
  assert.equal(events[0].external_customer_id, "acme");
  assert.equal(events[0].external_subscription_id, "sub_acme");
  assert.equal(events[0].properties.op, "+1");
});

test("toRagoEvents: a public repo creation bills nothing", () => {
  const payload = { action: "created", repository: { full_name: "acme/widgets", private: false, owner: { username: "acme" } } };
  assert.deepEqual(toRagoEvents("repository", payload, "d2"), []);
});

test("toRagoEvents: deleting a private repo bills -1", () => {
  const payload = { action: "deleted", repository: { full_name: "acme/widgets", private: true, owner: { username: "acme" } } };
  const events = toRagoEvents("repository", payload, "d3");
  assert.equal(events[0].properties.op, "-1");
});

test("toRagoEvents: member add/remove bills a seat event", () => {
  const payload = { action: "added", user: { username: "bob" }, organization: { username: "acme" } };
  const events = toRagoEvents("member", payload, "d4");
  assert.equal(events[0].code, "rithub_seat");
  assert.equal(events[0].properties.user, "bob");
});

test("toRagoEvents: push bills CI usage with a commit count", () => {
  const payload = { repository: { full_name: "acme/widgets", owner: { username: "acme" } }, commits: [{}, {}, {}] };
  const events = toRagoEvents("push", payload, "d5");
  assert.equal(events[0].code, "rithub_ci_push");
  assert.equal(events[0].properties.commits, 3);
});

test("toRagoEvents: unknown event types bill nothing", () => {
  assert.deepEqual(toRagoEvents("issues", {}, "d6"), []);
});

test("isSettleableInvoice: only finalized, positive, non-draft invoices settle", () => {
  assert.equal(isSettleableInvoice("invoice.created", { status: "finalized", total_amount_cents: 500 }), true);
  assert.equal(isSettleableInvoice("invoice.created", { status: "draft", total_amount_cents: 500 }), false);
  assert.equal(isSettleableInvoice("invoice.created", { status: "voided", total_amount_cents: 500 }), false);
  assert.equal(isSettleableInvoice("invoice.created", { status: "finalized", total_amount_cents: 0 }), false);
  assert.equal(isSettleableInvoice("invoice.payment_failure", { status: "finalized", total_amount_cents: 500 }), false);
});

test("resolveOrgAccount: multi-tenant map resolves per-org, unmapped orgs fail closed", () => {
  const env = { MINT_ORG_ACCT_MAP: JSON.stringify({ acme: "acct_acme" }) };
  assert.equal(resolveOrgAccount("acme", env), "acct_acme");
  assert.equal(resolveOrgAccount("widgetco", env), undefined);
  assert.equal(resolveOrgAccount("", env), undefined);
});

test("resolveOrgAccount: legacy single-tenant fallback only applies to the named org", () => {
  const env = { MINT_ORG_ACCT: "acct_1", MINT_ORG_ID: "acme" };
  assert.equal(resolveOrgAccount("acme", env), "acct_1");
  assert.equal(resolveOrgAccount("other-org", env), undefined);
});

test("resolveOrgAccount: malformed JSON map fails closed instead of throwing", () => {
  const env = { MINT_ORG_ACCT_MAP: "{not json" };
  assert.equal(resolveOrgAccount("acme", env), undefined);
});
