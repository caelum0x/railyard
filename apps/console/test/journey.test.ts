// The money-ladder journey end to end, against a Dodo test-mode fake and a
// Scribase Auth fake (RYP-D1 acceptance 1 to 3 and 5).
import { test } from "node:test";
import assert from "node:assert/strict";

import { addApp, AppError, installSnippet } from "../lib/apps.ts";
import { BillingError, cancel, setPaused, startCheckout } from "../lib/billing.ts";
import { Dodo } from "../lib/dodo.ts";
import { access } from "../lib/ladder.ts";
import { MemoryRepo } from "../lib/repo.ts";
import { finishSignIn, startSignIn } from "../lib/scribase-auth.ts";
import { openSession } from "../lib/session.ts";
import { completeSignIn } from "../lib/signin.ts";
import { handleDodoWebhook } from "../lib/webhook.ts";
import { addAppFlow, clicksToSnippet } from "../lib/add-app-flow.ts";
import { fakeDodo, fakeScribaseAuth, signedWebhook, SESSION_SECRET, WEBHOOK_SECRET } from "./helpers.ts";

const cfg = {
  consoleUrl: "https://railyard.ereboslab.com",
  sessionSecret: SESSION_SECRET,
  products: { cardCheck: "pdt_card", month: "pdt_month", year: "pdt_year" },
};

async function signIn(repo: MemoryRepo, now: Date) {
  const auth = fakeScribaseAuth();
  const start = await startSignIn({ scribaseUrl: "https://ry.scribase.com", consoleUrl: cfg.consoleUrl, provider: "google" });
  assert.match(start.url, /^https:\/\/ry\.scribase\.com\/auth\/v1\/authorize\?provider=google&redirect_to=.*code_challenge_method=s256$/);
  const user = await finishSignIn({ scribaseUrl: "https://ry.scribase.com", anonKey: "anon", code: "good-code", verifier: start.verifier, fetchImpl: auth.fetchImpl });
  return completeSignIn(repo, SESSION_SECRET, user, now);
}

test("sign-in -> $1 test checkout -> entitlements row within 30 s -> snippet in 3 clicks or fewer", async () => {
  const t0 = Date.now();
  let now = new Date("2026-09-29T12:00:00Z");
  const repo = new MemoryRepo(() => now);
  const dodoFake = fakeDodo();
  const dodo = new Dodo({ base: "https://test.dodopayments.com", apiKey: "test_key", allowLive: false, fetchImpl: dodoFake.fetchImpl });

  const { org, cookie } = await signIn(repo, now);
  const session = await openSession(SESSION_SECRET, cookie, now);
  assert.equal(session?.org, org.id);
  assert.equal(session?.email, "founder@example.com");
  assert.equal(access(await repo.getEntitlement(org.id), now).state, "no_card");
  await assert.rejects(addApp(repo, org.id, "my-app", now), (e: AppError) => e.code === "no_plan");

  const url = await startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: session!.email }, "card_check");
  assert.match(url, /^https:\/\/test\.checkout\.dodopayments\.com\//);
  const checkout = dodoFake.calls.find((c) => c.url.endsWith("/checkouts"))!;
  assert.equal(checkout.body.product_cart[0].product_id, "pdt_card");

  // Dodo test mode pays and calls the webhook with the checkout metadata copied onto the payment.
  now = new Date(now.getTime() + 5_000);
  const wh = await signedWebhook("payment.succeeded", { payment_id: "pay_1", total_amount: 100, customer: { customer_id: "cus_1" }, metadata: checkout.body.metadata }, now);
  const deps = { repo, dodo, webhookSecret: WEBHOOK_SECRET, sessionSecret: SESSION_SECRET, brandId: null, now: () => now };
  const res = await handleDodoWebhook(deps, wh.headers, wh.body);
  assert.equal(res.status, 200, JSON.stringify(res.body));
  const ent = await repo.getEntitlement(org.id);
  assert.equal(ent?.plan, "trial");
  assert.equal(ent?.credit_cents, 100);
  assert.equal(ent?.apps_limit, 3);
  const credit = dodoFake.calls.find((c) => c.url.endsWith("/wallets/ledger-entries"))!;
  assert.deepEqual([credit.body.entry_type, credit.body.amount, credit.body.currency], ["credit", 100, "USD"]);
  assert.ok(Date.parse(ent!.updated_at) - Date.parse(org.created_at) <= 30_000, "entitlements row within 30 s of sign-in");

  // Apps screen: "Add app" (click 1), type a name, "Create" (click 2) -> snippet.
  const flow = await clicksToSnippet(addAppFlow, async (name) => {
    const app = await addApp(repo, org.id, name, now);
    return installSnippet(app, "https://insights.railyard.ereboslab.com");
  }, "My App");
  assert.ok(flow.clicks <= 3, `took ${flow.clicks} clicks`);
  assert.match(flow.snippet!.script, /data-site="ry_site_/);
  assert.equal(flow.snippet!.deploy, "npx railyard deploy --app my-app");
  assert.ok(dodoFake.calls.every((c) => c.url.startsWith("https://test.dodopayments.com/")), "no live Dodo call");
  assert.ok(Date.now() - t0 < 30_000);
});

test("a test-mode refund of the card check reverses the credit and ends the trial", async () => {
  let now = new Date("2026-09-29T12:00:00Z");
  const repo = new MemoryRepo(() => now);
  const dodoFake = fakeDodo();
  const dodo = new Dodo({ base: "https://test.dodopayments.com", apiKey: "k", allowLive: false, fetchImpl: dodoFake.fetchImpl });
  const { org } = await signIn(repo, now);
  await startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "founder@example.com" }, "card_check");
  const meta = dodoFake.calls[0].body.metadata;
  const deps = { repo, dodo, webhookSecret: WEBHOOK_SECRET, sessionSecret: SESSION_SECRET, brandId: null, now: () => now };
  const paid = await signedWebhook("payment.succeeded", { payment_id: "pay_9", customer: { customer_id: "cus_9" }, metadata: meta }, now);
  assert.equal((await handleDodoWebhook(deps, paid.headers, paid.body)).status, 200);
  now = new Date(now.getTime() + 60_000);
  const refund = await signedWebhook("refund.succeeded", { refund_id: "ref_1", payment_id: "pay_9" }, now);
  const res = await handleDodoWebhook(deps, refund.headers, refund.body);
  assert.equal(res.status, 200, JSON.stringify(res.body));
  const ent = await repo.getEntitlement(org.id);
  assert.equal(ent?.credit_cents, 0);
  assert.equal(ent?.card_verified_at, null);
  assert.equal(ent?.plan, "none");
  const debit = dodoFake.calls.filter((c) => c.url.endsWith("/wallets/ledger-entries")).at(-1)!;
  assert.deepEqual([debit.body.entry_type, debit.body.amount, debit.body.idempotency_key], ["debit", 100, "railyard-card-check-debit:pay_9"]);
});

test("subscription, paused switch and one-click cancel", async () => {
  let now = new Date("2026-09-29T12:00:00Z");
  const repo = new MemoryRepo(() => now);
  const dodoFake = fakeDodo();
  const dodo = new Dodo({ base: "https://test.dodopayments.com", apiKey: "k", allowLive: false, fetchImpl: dodoFake.fetchImpl });
  const { org } = await signIn(repo, now);
  await startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "plan", "year");
  const co = dodoFake.calls[0].body;
  assert.equal(co.product_cart[0].product_id, "pdt_year");
  const deps = { repo, dodo, webhookSecret: WEBHOOK_SECRET, sessionSecret: SESSION_SECRET, brandId: null, now: () => now };
  const active = await signedWebhook("subscription.active", { subscription_id: "sub_1", customer: { customer_id: "cus_2" }, metadata: co.metadata }, now);
  assert.equal((await handleDodoWebhook(deps, active.headers, active.body)).status, 200);
  let ent = await repo.getEntitlement(org.id);
  assert.deepEqual([ent?.plan, ent?.status, ent?.interval], ["railyard", "active", "year"]);

  ent = await setPaused({ repo, dodo }, org.id, true, now);
  assert.equal(ent.paused, true);
  assert.deepEqual(dodoFake.calls.at(-1)!.body, { status: "paused" });
  assert.equal(access(ent, now).canAddApps, false);
  ent = await setPaused({ repo, dodo }, org.id, false, now);
  assert.deepEqual(dodoFake.calls.at(-1)!.body, { status: "active" });
  assert.equal(ent.status, "active");

  ent = await cancel({ repo, dodo }, org.id, now);
  assert.equal(ent.cancel_at_period_end, true);
  assert.deepEqual(dodoFake.calls.at(-1)!.body, { cancel_at_next_billing_date: true });
  assert.equal(access(ent, now).state, "cancelling");
  const ended = await signedWebhook("subscription.cancelled", { subscription_id: "sub_1", metadata: co.metadata }, now);
  await handleDodoWebhook(deps, ended.headers, ended.body);
  assert.equal(access(await repo.getEntitlement(org.id), now).state, "cancelled");
});

test("the live payment host is refused unless the owner switches it on", () => {
  assert.throws(() => new Dodo({ base: "https://live.dodopayments.com", apiKey: "k", allowLive: false }), /test mode/);
});

test("no second charge: a subscribed org cannot open another plan checkout, a card on file cannot pay the card check again", async () => {
  const now = new Date("2026-09-29T12:00:00Z");
  const repo = new MemoryRepo(() => now);
  const dodoFake = fakeDodo();
  const dodo = new Dodo({ base: "https://test.dodopayments.com", apiKey: "k", allowLive: false, fetchImpl: dodoFake.fetchImpl });
  const { org } = await signIn(repo, now);
  const deps = { repo, dodo, webhookSecret: WEBHOOK_SECRET, sessionSecret: SESSION_SECRET, brandId: null, now: () => now };
  await startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "card_check");
  const paid = await signedWebhook("payment.succeeded", { payment_id: "pay_x", customer: { customer_id: "cus_x" }, metadata: dodoFake.calls[0].body.metadata }, now);
  await handleDodoWebhook(deps, paid.headers, paid.body);
  await assert.rejects(startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "card_check"), (e: BillingError) => e.code === "card_on_file");

  await startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "plan", "month");
  const planMeta = dodoFake.calls.filter((c) => c.url.endsWith("/checkouts")).at(-1)!.body.metadata;
  const active = await signedWebhook("subscription.active", { subscription_id: "sub_x", customer: { customer_id: "cus_x" }, metadata: planMeta }, now);
  await handleDodoWebhook(deps, active.headers, active.body);
  const before = dodoFake.calls.length;
  for (const interval of ["month", "year"] as const) {
    await assert.rejects(startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "plan", interval), (e: BillingError) => e.code === "already_subscribed");
  }
  await cancel({ repo, dodo }, org.id, now);
  await assert.rejects(startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "plan", "year"), (e: BillingError) => e.code === "already_subscribed", "cancelling still bills until the period ends");
  assert.equal(dodoFake.calls.filter((c) => c.url.endsWith("/checkouts")).length, 2);
  assert.ok(dodoFake.calls.length >= before);

  const ended = await signedWebhook("subscription.cancelled", { subscription_id: "sub_x", metadata: planMeta }, now);
  await handleDodoWebhook(deps, ended.headers, ended.body);
  const url = await startCheckout({ repo, dodo, cfg }, { orgId: org.id, email: "f@example.com" }, "plan", "month");
  assert.match(url, /^https:\/\/test\.checkout\.dodopayments\.com\//, "after it ends, subscribing again works");
});
