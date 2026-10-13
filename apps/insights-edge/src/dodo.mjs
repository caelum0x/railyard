// Railyard Insights paid plan: a Dodo Payments checkout that raises a site's
// stored-events cap, and the webhook that confirms the payment and applies
// it. Test mode by default (DODO_API_BASE defaults to the test host); the
// live host is refused unless the owner sets RAILYARD_DODO_LIVE=1, mirroring
// apps/console/lib/dodo.ts and lib/config.ts so the two products agree.
//
// Nothing here claims to be live: billingReady() is the single gate the
// dashboard and the webhook route both check, and it is false until
// DODO_API_KEY, DODO_PRODUCT_ID and DODO_WEBHOOK_SECRET are all set.

import { FREE_EVENTS_PER_MONTH } from "./selfserve.mjs";

export const DODO_TEST_BASE = "https://test.dodopayments.com";
export const DODO_LIVE_BASE = "https://live.dodopayments.com";
export const PAID_EVENTS_PER_MONTH = 250_000;
const WEBHOOK_SERVICE = "railyard-insights";
const TOLERANCE_SECONDS = 300;

/** The Dodo API host to call, or null when a live host is requested without the owner's switch. */
function dodoBase(env) {
  const base = String(env.DODO_API_BASE ?? DODO_TEST_BASE).replace(/\/$/, "");
  if (base === DODO_LIVE_BASE && env.RAILYARD_DODO_LIVE !== "1") return null;
  return base;
}

/** True only once the key, the product and the webhook secret are all set and the host is resolvable. */
export function billingReady(env) {
  return Boolean(env.DODO_API_KEY && env.DODO_PRODUCT_ID && env.DODO_WEBHOOK_SECRET && dodoBase(env));
}

/**
 * Starts a Dodo checkout for one site's paid plan.
 * @returns {Promise<{ok: true, checkoutUrl: string} | {ok: false, error: string}>}
 */
export async function createCheckout(env, { siteKey, domain, email, returnUrl }, fetcher = fetch) {
  if (!billingReady(env)) return { ok: false, error: "billing_not_configured" };
  const base = dodoBase(env);
  let res;
  try {
    res = await fetcher(`${base}/checkouts`, {
      method: "POST",
      headers: { authorization: `Bearer ${env.DODO_API_KEY}`, "content-type": "application/json" },
      body: JSON.stringify({
        product_cart: [{ product_id: env.DODO_PRODUCT_ID, quantity: 1 }],
        customer: { email, name: domain },
        minimal_address: true,
        metadata: { service: WEBHOOK_SERVICE, site_key: siteKey },
        return_url: returnUrl,
      }),
    });
  } catch {
    return { ok: false, error: "checkout_request_failed" };
  }
  const text = await res.text();
  if (!res.ok) return { ok: false, error: `checkout_failed_${res.status}` };
  let out;
  try {
    out = text ? JSON.parse(text) : {};
  } catch {
    return { ok: false, error: "checkout_bad_response" };
  }
  if (typeof out.checkout_url !== "string" || !/^https:\/\//.test(out.checkout_url)) return { ok: false, error: "checkout_no_url" };
  return { ok: true, checkoutUrl: out.checkout_url };
}

function b64decode(text) {
  try {
    const bin = atob(text);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

function b64encode(bytes) {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

async function hmacSha256(key, data) {
  const enc = new TextEncoder();
  const k = await crypto.subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await crypto.subtle.sign("HMAC", k, enc.encode(data)));
}

function constantTimeEqual(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

/** The `webhook-signature` header value Dodo would send for this body — used by tests (and the one-off replay tool). */
export async function signDodoWebhook(secret, id, timestamp, body) {
  const key = b64decode(String(secret).trim().replace(/^whsec_/, ""));
  if (!key || key.length === 0) throw new Error("webhook secret is not valid base64");
  return `v1,${b64encode(await hmacSha256(key, `${id}.${timestamp}.${body}`))}`;
}

/**
 * Standard Webhooks verification (the scheme Dodo signs with): signed content
 * `${id}.${timestamp}.${body}`, HMAC-SHA256 with the base64 key after the
 * `whsec_` prefix, any `v1,<base64>` in `webhook-signature` accepted.
 */
export async function verifyDodoWebhook(secret, headers, body, nowSeconds = Math.floor(Date.now() / 1000)) {
  if (!secret || !headers?.id || !headers?.timestamp || !headers?.signature) return false;
  const sent = Number(String(headers.timestamp).trim());
  if (!Number.isInteger(sent) || Math.abs(nowSeconds - sent) > TOLERANCE_SECONDS) return false;
  const key = b64decode(String(secret).trim().replace(/^whsec_/, ""));
  if (!key || key.length === 0) return false;
  const expected = b64encode(await hmacSha256(key, `${headers.id}.${String(headers.timestamp).trim()}.${body}`));
  return String(headers.signature)
    .split(/\s+/)
    .filter((c) => c.startsWith("v1,"))
    .some((c) => constantTimeEqual(c.slice(3), expected));
}

const RAISE_TYPES = new Set(["payment.succeeded", "subscription.active", "subscription.renewed", "subscription.plan_changed"]);
const LOWER_TYPES = new Set(["subscription.cancelled", "subscription.expired", "subscription.failed"]);

/**
 * Applies one verified Dodo event to the site it names in its metadata:
 * raises the free site's events_limit to the paid plan, or drops it back to
 * free once the subscription ends. Idempotent — replaying the same event
 * (or an event after the plan already matches) is a no-op.
 * @returns {Promise<{applied: boolean, reason: string}>}
 */
export async function applyWebhookEvent(env, event) {
  const type = event?.type;
  const data = event?.data;
  if (typeof type !== "string" || !data || typeof data !== "object") return { applied: false, reason: "invalid_body" };
  const meta = data.metadata;
  const siteKey = meta && typeof meta === "object" ? meta.site_key : null;
  if (!meta || typeof meta !== "object" || meta.service !== WEBHOOK_SERVICE || typeof siteKey !== "string" || !siteKey) {
    return { applied: false, reason: "not_insights" };
  }
  if (!env.SITES) return { applied: false, reason: "no_site_store" };
  const raw = await env.SITES.get(`site:${siteKey}`);
  if (!raw) return { applied: false, reason: "unknown_site" };
  const site = JSON.parse(raw);

  if (RAISE_TYPES.has(type)) {
    if (site.paid === true && site.events_limit === PAID_EVENTS_PER_MONTH) return { applied: true, reason: "already_paid" };
    await env.SITES.put(`site:${siteKey}`, JSON.stringify({ ...site, events_limit: PAID_EVENTS_PER_MONTH, paid: true }));
    return { applied: true, reason: "raised" };
  }
  if (LOWER_TYPES.has(type)) {
    if (!site.paid) return { applied: true, reason: "already_free" };
    await env.SITES.put(`site:${siteKey}`, JSON.stringify({ ...site, events_limit: FREE_EVENTS_PER_MONTH, paid: false }));
    return { applied: true, reason: "lowered" };
  }
  return { applied: false, reason: "ignored_type" };
}
