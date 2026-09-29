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

test("a refund of an older card check takes its dollar back but keeps the current card", () => {
  let { ent } = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p1", customerId: "c", at: t0 });
  ent = applyLadder(ent, { kind: "card_check_paid", paymentId: "p2", customerId: "c", at: t0 }).ent;
  assert.equal(ent.credit_cents, 200);
  const r = applyLadder(ent, { kind: "card_check_refunded", paymentId: "p1", at: t0 });
  assert.deepEqual([r.ent.credit_cents, r.ent.card_check_payment_id, r.ent.plan], [100, "p2", "trial"]);
  assert.deepEqual(r.effects.map((e) => [e.kind, e.idempotencyKey]), [["wallet_debit", "railyard-card-check-debit:p1"]]);
});

test("one trial per org: a second card check never restarts or extends it", () => {
  const day = 86_400_000;
  const first = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p1", customerId: "c", at: t0 }).ent;
  const later = new Date(t0.getTime() + 13 * day);
  const second = applyLadder(first, { kind: "card_check_paid", paymentId: "p2", customerId: "c", at: later }).ent;
  assert.equal(second.trial_ends_at, first.trial_ends_at, "trial end unchanged");
  assert.equal(access(second, new Date(t0.getTime() + 15 * day)).state, "trial_ended");
});

test("refund then pay again: no second trial, the next step is the plan", () => {
  const paid = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p1", customerId: "c", at: t0 }).ent;
  const at = new Date(t0.getTime() + 10 * 86_400_000);
  const refunded = applyLadder(paid, { kind: "card_check_refunded", paymentId: "p1", at }).ent;
  assert.equal(access(refunded, at).state, "no_card");
  assert.ok(refunded.trial_ends_at, "the trial is marked used");
  const again = applyLadder(refunded, { kind: "card_check_paid", paymentId: "p2", customerId: "c", at }).ent;
  assert.equal(again.plan, "none");
  assert.equal(again.apps_limit, 0);
  const a = access(again, at);
  assert.deepEqual([a.state, a.canAddApps], ["trial_ended", false]);
});

test("cancel a trial, or end a subscription, then pay the card check: no new trial", () => {
  const trial = applyLadder(emptyEntitlement("o", t0), { kind: "card_check_paid", paymentId: "p1", customerId: "c", at: t0 }).ent;
  const cancelled = endTrial(trial, t0);
  const again = applyLadder(cancelled, { kind: "card_check_paid", paymentId: "p2", customerId: "c", at: t0 }).ent;
  assert.equal(access(again, t0).canAddApps, false);

  const direct = applyLadder(emptyEntitlement("o2", t0), { kind: "subscription_active", subscriptionId: "s", customerId: "c", interval: "month", at: t0 }).ent;
  const ended = applyLadder(direct, { kind: "subscription_ended", at: t0 }).ent;
  const paid = applyLadder(ended, { kind: "card_check_paid", paymentId: "p3", customerId: "c", at: t0 }).ent;
  assert.equal(paid.plan, "none");
  assert.equal(access(paid, t0).canAddApps, false);
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
