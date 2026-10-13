// Railyard Insights paid plan (2026-10-14): a Dodo checkout that raises a
// near-cap site's stored-events limit, and the webhook that confirms it.
// Unit tests on the pure Dodo helpers (no network), then the full dashboard
// -> checkout -> webhook loop in Miniflare with a mocked Dodo.
import assert from "node:assert/strict";
import { test } from "node:test";
import { applyWebhookEvent, billingReady, createCheckout, PAID_EVENTS_PER_MONTH, signDodoWebhook, verifyDodoWebhook } from "../src/dodo.mjs";
import { FREE_EVENTS_PER_MONTH } from "../src/selfserve.mjs";
import { ev, sleep, startCollector } from "./harness.mjs";

const SECRET = `whsec_${Buffer.from("a-test-webhook-signing-secret-32b").toString("base64")}`;

const DODO_ENV = { DODO_API_KEY: "test_key", DODO_PRODUCT_ID: "pdt_insights_pro", DODO_WEBHOOK_SECRET: SECRET };

function fakeKv(initial = {}) {
  const store = new Map(Object.entries(initial));
  return {
    async get(key) {
      return store.has(key) ? store.get(key) : null;
    },
    async put(key, value) {
      store.set(key, value);
    },
    store,
  };
}

test("billingReady is false until the key, product and webhook secret are all set", () => {
  assert.equal(billingReady({}), false);
  assert.equal(billingReady({ DODO_API_KEY: "k" }), false);
  assert.equal(billingReady({ ...DODO_ENV }), true);
  for (const name of Object.keys(DODO_ENV)) {
    const env = { ...DODO_ENV, [name]: "" };
    assert.equal(billingReady(env), false, `${name} missing`);
  }
});

test("the live Dodo host is refused without the owner's exact RAILYARD_DODO_LIVE=1", () => {
  const live = { ...DODO_ENV, DODO_API_BASE: "https://live.dodopayments.com" };
  assert.equal(billingReady(live), false);
  for (const value of ["true", "yes", "01", " 1", ""]) {
    assert.equal(billingReady({ ...live, RAILYARD_DODO_LIVE: value }), false, `RAILYARD_DODO_LIVE=${JSON.stringify(value)} must not unlock the live host`);
  }
  assert.equal(billingReady({ ...live, RAILYARD_DODO_LIVE: "1" }), true);
});

test("createCheckout posts the product, the signed metadata and the return url, and reads back checkout_url", async () => {
  const calls = [];
  const fetcher = async (url, init) => {
    calls.push({ url, body: JSON.parse(init.body) });
    return new Response(JSON.stringify({ checkout_url: "https://test.dodopayments.com/checkout/abc" }), { status: 200 });
  };
  const r = await createCheckout(DODO_ENV, { siteKey: "s_abc", domain: "shop.example", email: "a@shop.example", returnUrl: "https://insights.test/d/tok?upgraded=1" }, fetcher);
  assert.deepEqual(r, { ok: true, checkoutUrl: "https://test.dodopayments.com/checkout/abc" });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "https://test.dodopayments.com/checkouts");
  assert.deepEqual(calls[0].body.product_cart, [{ product_id: "pdt_insights_pro", quantity: 1 }]);
  assert.equal(calls[0].body.customer.email, "a@shop.example");
  assert.deepEqual(calls[0].body.metadata, { service: "railyard-insights", site_key: "s_abc" });
});

test("createCheckout refuses to run when billing is not configured, and surfaces a bad response", async () => {
  assert.deepEqual(await createCheckout({}, { siteKey: "s_1", domain: "d", email: "e@d", returnUrl: "u" }), { ok: false, error: "billing_not_configured" });
  const noUrl = async () => new Response(JSON.stringify({}), { status: 200 });
  assert.deepEqual(await createCheckout(DODO_ENV, { siteKey: "s_1", domain: "d", email: "e@d", returnUrl: "u" }, noUrl), { ok: false, error: "checkout_no_url" });
  const failing = async () => new Response("nope", { status: 500 });
  assert.deepEqual(await createCheckout(DODO_ENV, { siteKey: "s_1", domain: "d", email: "e@d", returnUrl: "u" }, failing), { ok: false, error: "checkout_failed_500" });
});

test("verifyDodoWebhook accepts a correctly signed, fresh body and rejects a tampered one, an old one, and a wrong secret", async () => {
  const body = JSON.stringify({ type: "payment.succeeded", data: { metadata: { service: "railyard-insights", site_key: "s_1" } } });
  const now = Math.floor(Date.now() / 1000);
  const headers = { id: "evt_1", timestamp: String(now), signature: await signDodoWebhook(SECRET, "evt_1", String(now), body) };
  assert.equal(await verifyDodoWebhook(SECRET, headers, body, now), true);
  assert.equal(await verifyDodoWebhook(SECRET, headers, body + "x", now), false);
  assert.equal(await verifyDodoWebhook(SECRET, headers, body, now + 10_000), false);
  assert.equal(await verifyDodoWebhook("whsec_" + Buffer.from("a-different-secret-entirely-32by").toString("base64"), headers, body, now), false);
  assert.equal(await verifyDodoWebhook(SECRET, { id: null, timestamp: headers.timestamp, signature: headers.signature }, body, now), false);
});

test("applyWebhookEvent raises a known site on payment/subscription events, idempotently, and ignores anything not ours", async () => {
  const kv = fakeKv({ "site:s_1": JSON.stringify({ label: "shop.example", events_limit: FREE_EVENTS_PER_MONTH }) });
  const env = { SITES: kv };
  const okEvent = (type) => ({ type, data: { metadata: { service: "railyard-insights", site_key: "s_1" } } });

  assert.deepEqual(await applyWebhookEvent(env, okEvent("payment.succeeded")), { applied: true, reason: "raised" });
  const site = JSON.parse(await kv.get("site:s_1"));
  assert.equal(site.events_limit, PAID_EVENTS_PER_MONTH);
  assert.equal(site.paid, true);
  assert.equal(site.label, "shop.example");

  // Replaying the same confirmation (or a renewal) is a no-op, not a second raise.
  assert.deepEqual(await applyWebhookEvent(env, okEvent("subscription.renewed")), { applied: true, reason: "already_paid" });

  assert.deepEqual(await applyWebhookEvent(env, okEvent("subscription.cancelled")), { applied: true, reason: "lowered" });
  assert.equal(JSON.parse(await kv.get("site:s_1")).events_limit, FREE_EVENTS_PER_MONTH);
  assert.equal(JSON.parse(await kv.get("site:s_1")).paid, false);
  assert.deepEqual(await applyWebhookEvent(env, okEvent("subscription.expired")), { applied: true, reason: "already_free" });

  assert.deepEqual(await applyWebhookEvent(env, { type: "payment.succeeded", data: { metadata: { service: "some-other-product", site_key: "s_1" } } }), { applied: false, reason: "not_insights" });
  assert.deepEqual(await applyWebhookEvent(env, { type: "payment.succeeded", data: { metadata: { service: "railyard-insights", site_key: "s_missing" } } }), { applied: false, reason: "unknown_site" });
  assert.deepEqual(await applyWebhookEvent(env, okEvent("checkout.opened")), { applied: false, reason: "ignored_type" });
  assert.deepEqual(await applyWebhookEvent(env, { type: "payment.succeeded" }), { applied: false, reason: "invalid_body" });
});

const sites = { ry_site_owned001: { org_id: "org-1", app_id: "bidstage", label: "bidstage.app", events_limit: 100_000 } };
const baseBindings = { INGEST_URL: "", STATS_TOKEN: "stats-token-123", RALLY_TX_URL: "https://mail.test/send", RALLY_TX_KEY: "smail_test", REPORT_TO: "owner@example.com" };
const signup = (c, body) => fetch(`${c.base}/api/sites`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });

test("without Dodo env the dashboard never offers an upgrade, even at the cap, and the webhook route is off", async () => {
  const c = await startCollector({ d1: true, sites, bindings: baseBindings, respond: () => 200 });
  try {
    const s = await (await signup(c, { email: "a@shop.example", domain: "shop.example" })).json();
    const token = s.dashboard_url.split("/d/")[1];
    const kv = await c.mf.getKVNamespace("SITES");
    const site = JSON.parse(await kv.get(`site:${s.site_key}`));
    await kv.put(`site:${s.site_key}`, JSON.stringify({ ...site, events_limit: 2 }));
    await c.send(ev(s.site_key, "/", { hostname: "shop.example" }));
    await c.send(ev(s.site_key, "/2", { hostname: "shop.example" }));
    await sleep(3500);

    const page = await fetch(`${c.base}/d/${token}`);
    const html = await page.text();
    assert.match(html, /stored events used this month/);
    assert.doesNotMatch(html, /Upgrade for more stored events/);

    const up = await fetch(`${c.base}/d/${token}/upgrade`, { method: "POST" });
    assert.equal(up.status, 503);
    const hook = await fetch(`${c.base}/api/billing/webhook`, { method: "POST", body: "{}" });
    assert.equal(hook.status, 503);
  } finally {
    await c.close();
  }
});

test("near the cap, with Dodo configured, the dashboard offers an upgrade that redirects to a real checkout, and the webhook raises the limit", async () => {
  const outboundResponses = { "/checkouts": { status: 200, body: { checkout_url: "https://test.dodopayments.com/checkout/sess_1" } } };
  const respond = (entry) => {
    const match = Object.keys(outboundResponses).find((suffix) => entry.url.endsWith(suffix));
    return match ? outboundResponses[match] : 200;
  };
  const c = await startCollector({ d1: true, sites, bindings: { ...baseBindings, ...DODO_ENV }, respond });
  try {
    const s = await (await signup(c, { email: "founder@shop.example", domain: "shop.example" })).json();
    const token = s.dashboard_url.split("/d/")[1];
    const kv = await c.mf.getKVNamespace("SITES");
    const before = JSON.parse(await kv.get(`site:${s.site_key}`));
    assert.equal(before.events_limit, FREE_EVENTS_PER_MONTH);
    await kv.put(`site:${s.site_key}`, JSON.stringify({ ...before, events_limit: 2 }));
    await c.send(ev(s.site_key, "/", { hostname: "shop.example" }));
    await c.send(ev(s.site_key, "/2", { hostname: "shop.example" }));
    await sleep(3500);

    const page = await fetch(`${c.base}/d/${token}`);
    const html = await page.text();
    assert.match(html, /Upgrade for more stored events/);
    assert.match(html, new RegExp(`action="/d/${token}/upgrade"`));

    const up = await fetch(`${c.base}/d/${token}/upgrade`, { method: "POST", redirect: "manual" });
    assert.equal(up.status, 303);
    assert.equal(up.headers.get("location"), "https://test.dodopayments.com/checkout/sess_1");
    const checkoutCall = c.outbound.find((o) => o.url.endsWith("/checkouts"));
    assert.ok(checkoutCall);
    const sent = JSON.parse(checkoutCall.body);
    assert.deepEqual(sent.metadata, { service: "railyard-insights", site_key: s.site_key });
    assert.equal(sent.customer.email, "founder@shop.example");

    // Dodo confirms the payment with a signed webhook.
    const body = JSON.stringify({ type: "payment.succeeded", data: { payment_id: "pay_1", metadata: { service: "railyard-insights", site_key: s.site_key } } });
    const now = String(Math.floor(Date.now() / 1000));
    const headers = { "webhook-id": "evt_1", "webhook-timestamp": now, "webhook-signature": await signDodoWebhook(SECRET, "evt_1", now, body) };
    const hook = await fetch(`${c.base}/api/billing/webhook`, { method: "POST", headers, body });
    assert.equal(hook.status, 200);
    assert.deepEqual(await hook.json(), { received: true, applied: true, reason: "raised" });

    const after = JSON.parse(await kv.get(`site:${s.site_key}`));
    assert.equal(after.events_limit, PAID_EVENTS_PER_MONTH);
    assert.equal(after.paid, true);

    // A wrong signature is refused.
    const bad = await fetch(`${c.base}/api/billing/webhook`, { method: "POST", headers: { ...headers, "webhook-signature": "v1,not-a-real-signature" }, body });
    assert.equal(bad.status, 401);

    // Paid, the dashboard stops offering an upgrade and says so.
    const page2 = await fetch(`${c.base}/d/${token}`);
    const html2 = await page2.text();
    assert.doesNotMatch(html2, /Upgrade for more stored events/);
    assert.match(html2, /\(paid plan\)/);

    // Re-POSTing /upgrade once already paid just returns to the dashboard; no second checkout is created.
    const upAgain = await fetch(`${c.base}/d/${token}/upgrade`, { method: "POST", redirect: "manual" });
    assert.equal(upAgain.status, 303);
    assert.equal(upAgain.headers.get("location"), `/d/${token}`);
    assert.equal(c.outbound.filter((o) => o.url.endsWith("/checkouts")).length, 1);
  } finally {
    await c.close();
  }
});
