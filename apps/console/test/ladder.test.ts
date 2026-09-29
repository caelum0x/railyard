import { test } from "node:test";
import assert from "node:assert/strict";

import { access, applyLadder, endTrial } from "../lib/ladder.ts";
import { emptyEntitlement } from "../lib/types.ts";

const t0 = new Date("2026-09-29T00:00:00Z");

test("card check starts a 14-day trial with the plan's limits and credits $1 once", () => {
  const e0 = emptyEntitlement("o", t0);
  const { ent, effects } = applyLadder(e0, { kind: "card_check_paid", paymentId: "p", customerId: "c", at: t0 });
  assert.equal(e0.plan, "none", "input not mutated");
  assert.deepEqual([ent.plan, ent.apps_limit, ent.events_limit, ent.credit_cents], ["trial", 3, 100000, 100]);
  assert.equal(effects.length, 1);
  const again = applyLadder(ent, { kind: "card_check_paid", paymentId: "p", customerId: "c", at: t0 });
  assert.deepEqual(again.effects, []);
  assert.equal(again.ent.credit_cents, 100);
  assert.equal(access(ent, new Date(t0.getTime() + 13 * 86_400_000)).state, "trial");
  assert.equal(access(ent, new Date(t0.getTime() + 14 * 86_400_000 + 1)).state, "trial_ended");
});

test("a refund of another payment does not touch the card check", () => {
  const { ent } = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p", customerId: "c", at: t0 });
  const r = applyLadder(ent, { kind: "card_check_refunded", paymentId: "other", at: t0 });
  assert.equal(r.ent, ent);
  assert.deepEqual(r.effects, []);
});

test("a refund while subscribed keeps the subscription but removes the credit", () => {
  let { ent } = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p", customerId: "c", at: t0 });
  ent = applyLadder(ent, { kind: "subscription_active", subscriptionId: "s", customerId: "c", interval: "month", at: t0 }).ent;
  const r = applyLadder(ent, { kind: "card_check_refunded", paymentId: "p", at: t0 });
  assert.deepEqual([r.ent.plan, r.ent.credit_cents], ["railyard", 0]);
});

test("ending a trial removes access at once", () => {
  const { ent } = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p", customerId: null, at: t0 });
  assert.equal(access(endTrial(ent, t0), t0).canAddApps, false);
});
