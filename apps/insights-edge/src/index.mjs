// Railyard Insights edge collector (RYP-D3). Umami-compatible /api/send and
// /api/batch, site-key check, bot and prefetch drop, a salted daily visitor
// hash (the IP never leaves this function), plan caps, and 2 s batching to
// Insights ingest through one Durable Object per org.

import { dropReason } from "./bots.mjs";
import { MAX_BATCH, MAX_BODY_BYTES, parseOne } from "./payload.mjs";
import { TRACKER_JS } from "./tracker.mjs";
import { dailySalt, visitorHash } from "./visitor.mjs";
import { buildReport, sendReport, windows } from "./report.mjs";
import { capMessage } from "./caps.ts";
import { applyWebhookEvent, billingReady, createCheckout, verifyDodoWebhook } from "./dodo.mjs";
import { createSite, deleteSite, hostMatches, resolveDashboard, signupEmail, usage } from "./selfserve.mjs";
import { pageSpeed, siteStats } from "./store.mjs";
import { ROBOTS, SITEMAP, cookielessAnalyticsPage, dashboardPage, fathomAlternativePage, googleAnalyticsAlternativePage, landingPage, messagePage, plausibleAlternativePage, privacyPage } from "./pages.mjs";
import { REPLAY_JS, REPLAY_VIEWER_JS } from "./replay/tracker.mjs";
import { ingestReplayChunk } from "./replay/ingest.mjs";
import { replayDashboardRoute, replayEventsApi, replayPublicConfig } from "./replay/routes.mjs";
import { RRWEB_RECORDER_JS } from "./vendor/rrweb-recorder.generated.mjs";
import { RRWEB_PLAYER_JS } from "./vendor/rrweb-player.generated.mjs";

export { OrgCollector } from "./collector.mjs";

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "POST, OPTIONS",
  "access-control-allow-headers": "content-type",
  "access-control-max-age": "86400",
};

const reply = (status, body, extra = {}) =>
  new Response(body === null ? null : JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store", ...CORS, ...extra } });

// Site entries are written to KV by the console (key `site:<siteKey>`), cached here for a minute.
const siteCache = new Map();
async function site(env, key, now) {
  const hit = siteCache.get(key);
  if (hit && hit.until > now) return hit.value;
  const raw = await env.SITES.get(`site:${key}`);
  const value = raw ? JSON.parse(raw) : null;
  siteCache.set(key, { value, until: now + 60_000 });
  if (siteCache.size > 5000) siteCache.delete(siteCache.keys().next().value);
  return value;
}

async function collect(request, env, batch) {
  const drop = dropReason(request.headers);
  if (drop) return reply(202, { ok: true }, { "x-railyard-dropped": drop });
  if (!env.VISITOR_SALT_SECRET) return reply(503, { error: "collector not configured" });
  const text = await request.text();
  if (text.length > MAX_BODY_BYTES) return reply(413, { error: "too large" });
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch {
    return reply(400, { error: "invalid json" });
  }
  const items = batch ? parsed : [parsed];
  if (!Array.isArray(items) || !items.length || items.length > MAX_BATCH) return reply(400, { error: `send 1 to ${MAX_BATCH} events` });

  const now = Date.now();
  const salt = await dailySalt(env.VISITOR_SALT_SECRET, now);
  const ip = request.headers.get("cf-connecting-ip") ?? "";
  const ua = request.headers.get("user-agent") ?? "";
  const country = request.cf?.country ?? null;
  const bySite = new Map();
  for (const item of items) {
    const p = parseOne(item);
    if ("error" in p) continue;
    if (!bySite.has(p.siteKey)) bySite.set(p.siteKey, []);
    bySite.get(p.siteKey).push(p.event);
  }
  if (!bySite.size) return reply(400, { error: "no valid events" });

  let known = 0;
  let stored = 0;
  let capped = 0;
  let paused = 0;
  for (const [key, events] of bySite) {
    const s = await site(env, key, now);
    if (!s) continue;
    known += 1;
    if (s.paused) {
      paused += events.length;
      continue;
    }
    // A self-serve site stores events only from its own domain and subdomains.
    const own = s.hostname ? events.filter((e) => hostMatches(e.hostname, s.hostname)) : events;
    if (!own.length) continue;
    const visitor = await visitorHash({ salt, siteKey: key, ip, userAgent: ua });
    const ts = new Date(now).toISOString();
    const out = own.map((e) => ({ ...e, visitor, country, ts }));
    const stub = env.ORG_COLLECTOR.get(env.ORG_COLLECTOR.idFromName(s.org_id));
    const res = await stub.fetch("https://collector/ingest", {
      method: "POST",
      body: JSON.stringify({ orgId: s.org_id, site: { key, app_id: s.app_id, events_limit: s.events_limit }, events: out }),
    });
    const r = await res.json();
    stored += r.stored;
    capped += r.capped;
  }
  if (!known) return reply(404, { error: "unknown site" });
  const headers = {};
  if (capped) headers["x-railyard-capped"] = "1";
  if (paused) headers["x-railyard-paused"] = "1";
  return reply(202, { ok: true }, headers);
}

/** Constant-time-ish bearer check for the owner endpoints (stats, report). */
function authorized(request, env) {
  const want = env.STATS_TOKEN;
  const got = (request.headers.get("authorization") ?? "").replace(/^Bearer\s+/i, "");
  if (!want || got.length !== want.length) return false;
  let diff = 0;
  for (let i = 0; i < want.length; i += 1) diff |= want.charCodeAt(i) ^ got.charCodeAt(i);
  return diff === 0;
}

async function owner(request, env, url) {
  if (!env.DB) return reply(503, { error: "no event store" });
  if (!authorized(request, env)) return reply(401, { error: "unauthorized" });
  if (url.pathname === "/api/stats") {
    const days = Math.min(90, Math.max(1, Number(url.searchParams.get("days")) || 7));
    // `days` back from now (including today), so fresh events show up at once.
    return reply(200, await buildReport(env, Date.now() + 86_400_000, days));
  }
  return reply(200, await sendReport(env, Date.now()));
}

const DAY = 86_400_000;
const PAGE_HEADERS = {
  "content-type": "text/html; charset=utf-8",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
  "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'self'; connect-src 'self'; img-src 'self' data:; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
};
// The public pages also allow the zone's Cloudflare Web Analytics beacon (cookieless).
const PUBLIC_CSP = "default-src 'none'; style-src 'unsafe-inline'; script-src 'self' https://static.cloudflareinsights.com; connect-src 'self' https://cloudflareinsights.com; img-src 'self' data:; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";
const html = (status, body, extra = {}) => new Response(body, { status, headers: { ...PAGE_HEADERS, "cache-control": "no-store", ...extra } });
const landingTag = (env) => env.LANDING_SITE_KEY || null;

async function readSignup(request) {
  const type = request.headers.get("content-type") ?? "";
  if (type.includes("application/json")) {
    try {
      return { form: false, input: await request.json() };
    } catch {
      return { form: false, input: null };
    }
  }
  const f = await request.formData();
  return { form: true, input: { email: f.get("email"), domain: f.get("domain") } };
}

async function signup(request, env, url) {
  if ((Number(request.headers.get("content-length")) || 0) > 4096) return reply(413, { error: "too large" });
  const { form, input } = await readSignup(request);
  const r = await createSite(env, input, url.origin);
  if (!form) return reply(r.status, r.body);
  if (r.status !== 201) return html(r.status, landingPage({ error: r.body.error, values: input ?? {}, tag: landingTag(env) }), { "content-security-policy": PUBLIC_CSP });
  const token = r.body.dashboard_url.split("/d/")[1];
  return new Response(null, { status: 303, headers: { location: `/d/${token}?new=${r.body.mailed ? "mailed" : "1"}`, "cache-control": "no-store" } });
}

const WEBHOOK_MAX_BYTES = 65_536;

/** Dodo's confirmation that a site's checkout was paid (or its subscription ended). */
async function billingWebhook(request, env) {
  if (!billingReady(env)) return reply(503, { error: "billing_not_configured" });
  if ((Number(request.headers.get("content-length")) || 0) > WEBHOOK_MAX_BYTES) return reply(413, { error: "too large" });
  const body = await request.text();
  const headers = { id: request.headers.get("webhook-id"), timestamp: request.headers.get("webhook-timestamp"), signature: request.headers.get("webhook-signature") };
  if (!(await verifyDodoWebhook(env.DODO_WEBHOOK_SECRET, headers, body))) return reply(401, { error: "invalid_signature" });
  let event;
  try {
    event = JSON.parse(body);
  } catch {
    return reply(400, { error: "invalid_body" });
  }
  const r = await applyWebhookEvent(env, event);
  return reply(200, { received: true, ...r });
}

async function dashboardData(env, found, days) {
  const w = windows(Date.now() + DAY, days);
  const stats = await siteStats(env.DB, found.key, ...w.cur);
  const speed = await pageSpeed(env.DB, found.key, ...w.cur);
  const used = await usage(env.DB, found.key);
  return { stats, speed, usage: used, capLine: capMessage(used.month, found.site.events_limit ?? 0, new Date()) };
}

async function upgrade(request, env, url, token, found, priv) {
  if (request.method !== "POST") return html(405, messagePage("Not allowed", "Use the upgrade button on the dashboard."), priv);
  if (!billingReady(env)) return html(503, messagePage("Not available yet", "Card payments for Insights aren't turned on yet. Check back soon."), priv);
  if (found.site.paid) return new Response(null, { status: 303, headers: { location: `/d/${token}`, "cache-control": "no-store" } });
  const email = env.DB ? await signupEmail(env.DB, found.key) : null;
  if (!email) return html(400, messagePage("Not available", "This site has no sign-up email on file to send the receipt to."), priv);
  const r = await createCheckout(env, { siteKey: found.key, domain: found.site.label, email, returnUrl: `${url.origin}/d/${token}?upgraded=1` });
  if (!r.ok) return html(502, messagePage("Checkout failed", "The payment provider did not answer. Try again in a moment."), priv);
  return new Response(null, { status: 303, headers: { location: r.checkoutUrl, "cache-control": "no-store" } });
}

async function dashboard(request, env, url, parts) {
  const [, , token, action] = parts;
  const found = env.DB ? await resolveDashboard(env, token) : null;
  // no-transform keeps the zone's edge features from injecting third-party scripts into a private page.
  const priv = { "referrer-policy": "no-referrer", "x-robots-tag": "noindex, nofollow", "cache-control": "no-store, no-transform" };
  if (!found) {
    if (parts[1] === "api") return reply(404, { error: "not found" }, priv);
    return html(404, messagePage("Dashboard not found", "This link is not valid, or the site was deleted."), priv);
  }
  const days = [7, 30, 90].includes(Number(url.searchParams.get("days"))) ? Number(url.searchParams.get("days")) : 7;
  if (parts[1] === "api") {
    const d = await dashboardData(env, found, Math.min(90, Math.max(1, Number(url.searchParams.get("days")) || 7)));
    return reply(200, { domain: found.site.label, cur: d.stats, speed: d.speed, usage: d.usage, events_limit: found.site.events_limit, paid: Boolean(found.site.paid), billing_ready: billingReady(env) }, priv);
  }
  if (action === "delete") {
    if (request.method !== "POST") return html(405, messagePage("Not allowed", "Use the delete button on the dashboard."), priv);
    const f = await request.formData();
    if (String(f.get("confirm") ?? "").trim().toLowerCase() !== found.site.label) {
      return html(400, messagePage("Not deleted", `Type ${found.site.label} exactly to delete it.`, "error"), priv);
    }
    await deleteSite(env, found.key, found.site);
    return html(200, messagePage("Deleted", `${found.site.label} and every stored event are deleted. Remove the tag from your pages; it now does nothing.`, "ok"), priv);
  }
  if (action === "upgrade") return upgrade(request, env, url, token, found, priv);
  const d = await dashboardData(env, found, days);
  const fresh = url.searchParams.get("new");
  return html(
    200,
    dashboardPage({
      token,
      origin: url.origin,
      key: found.key,
      site: found.site,
      stats: d.stats,
      speed: d.speed,
      days,
      usage: d.usage,
      capLine: d.capLine,
      billingReady: billingReady(env),
      fresh: fresh === "mailed" ? "mailed" : fresh ? "yes" : url.searchParams.get("upgraded") ? "upgraded" : null,
    }),
    priv,
  );
}

/** Public pages, sign-up and dashboards; null when the path is not one of them. */
async function publicRoute(request, env, url) {
  const p = url.pathname;
  const get = request.method === "GET" || request.method === "HEAD";
  if (get && p === "/") return html(200, landingPage({ tag: landingTag(env) }), { "cache-control": "public, max-age=300", "content-security-policy": PUBLIC_CSP });
  if (get && p === "/privacy") return html(200, privacyPage({ tag: landingTag(env) }), { "cache-control": "public, max-age=300", "content-security-policy": PUBLIC_CSP });
  if (get && p === "/cookieless-analytics") return html(200, cookielessAnalyticsPage({ tag: landingTag(env) }), { "cache-control": "public, max-age=300", "content-security-policy": PUBLIC_CSP });
  if (get && p === "/google-analytics-alternative") return html(200, googleAnalyticsAlternativePage({ tag: landingTag(env) }), { "cache-control": "public, max-age=300", "content-security-policy": PUBLIC_CSP });
  if (get && p === "/plausible-alternative") return html(200, plausibleAlternativePage({ tag: landingTag(env) }), { "cache-control": "public, max-age=300", "content-security-policy": PUBLIC_CSP });
  if (get && p === "/fathom-analytics-alternative") return html(200, fathomAlternativePage({ tag: landingTag(env) }), { "cache-control": "public, max-age=300", "content-security-policy": PUBLIC_CSP });
  if (get && p === "/robots.txt") return new Response(ROBOTS, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "public, max-age=3600" } });
  if (get && p === "/sitemap.xml") return new Response(SITEMAP, { headers: { "content-type": "application/xml; charset=utf-8", "cache-control": "public, max-age=3600" } });
  if (request.method === "POST" && p === "/api/sites") return signup(request, env, url);
  if (request.method === "POST" && p === "/api/billing/webhook") return billingWebhook(request, env);
  const parts = p.split("/");
  if (parts[1] === "d" && parts[3] === "replay" && parts.length >= 4 && parts.length <= 5) return replayDashboardRoute(request, env, url, parts);
  if (parts[1] === "d" && parts.length >= 3 && parts.length <= 4 && (parts.length === 3 || parts[3] === "delete" || parts[3] === "upgrade")) return dashboard(request, env, url, parts);
  if (get && parts[1] === "api" && parts[2] === "d" && parts.length === 5 && parts[4] === "stats") return dashboard(request, env, url, ["", "api", parts[3]]);
  if (get && parts[1] === "api" && parts[2] === "d" && parts.length === 7 && parts[4] === "replay" && parts[6] === "events") return replayEventsApi(env, parts[3], parts[5]);
  return null;
}

export default {
  async scheduled(_controller, env, ctx) {
    ctx.waitUntil(
      sendReport(env, Date.now()).then((r) => {
        if (!r.sent) console.error(`insights-edge: weekly report not sent: ${JSON.stringify(r)}`);
      }),
    );
  },
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method === "OPTIONS") return new Response(null, { status: 204, headers: CORS });
    if (request.method === "GET" && url.pathname === "/s.js") {
      return new Response(TRACKER_JS, { headers: { "content-type": "application/javascript; charset=utf-8", "cache-control": "public, max-age=3600", ...CORS } });
    }
    // Session replay (2026-10-15): the opt-in tag, the vendored rrweb bundles it and the
    // player page load, and the two endpoints the tag talks to. Everything here is a
    // GET of static, versionless JS/CSS except /api/replay itself.
    if (request.method === "GET" && url.pathname === "/replay.js") {
      // Short-lived on purpose (unlike /s.js): a site owner who just flipped the replay toggle, or an
      // owner who just shipped a loader fix, should not be stuck on a stale cached copy for an hour.
      return new Response(REPLAY_JS, { headers: { "content-type": "application/javascript; charset=utf-8", "cache-control": "no-store", ...CORS } });
    }
    if (request.method === "GET" && url.pathname === "/replay-viewer.js") {
      return new Response(REPLAY_VIEWER_JS, { headers: { "content-type": "application/javascript; charset=utf-8", "cache-control": "no-store", ...CORS } });
    }
    if (request.method === "GET" && url.pathname === "/replay-recorder.js") {
      return new Response(RRWEB_RECORDER_JS, { headers: { "content-type": "application/javascript; charset=utf-8", "cache-control": "public, max-age=86400, immutable", ...CORS } });
    }
    if (request.method === "GET" && url.pathname === "/replay-player.js") {
      return new Response(RRWEB_PLAYER_JS, { headers: { "content-type": "application/javascript; charset=utf-8", "cache-control": "public, max-age=86400, immutable", ...CORS } });
    }
    if (request.method === "GET" && url.pathname === "/api/replay/config") {
      return replayPublicConfig(url.searchParams.get("site") ?? "", (key, now) => site(env, key, now), Date.now());
    }
    if (request.method === "POST" && url.pathname === "/api/replay") {
      return ingestReplayChunk(request, env, Date.now(), (key, now) => site(env, key, now));
    }
    if (request.method === "GET" && url.pathname === "/api/stats") return owner(request, env, url);
    if (request.method === "POST" && url.pathname === "/api/report") return owner(request, env, url);
    if (request.method === "POST" && url.pathname === "/api/send") return collect(request, env, false);
    if (request.method === "POST" && url.pathname === "/api/batch") return collect(request, env, true);
    const page = await publicRoute(request, env, url);
    if (page) return page;
    return reply(404, { error: "not found" });
  },
};
