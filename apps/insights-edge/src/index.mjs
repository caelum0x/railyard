// Railyard Insights edge collector (RYP-D3). Umami-compatible /api/send and
// /api/batch, site-key check, bot and prefetch drop, a salted daily visitor
// hash (the IP never leaves this function), plan caps, and 2 s batching to
// Insights ingest through one Durable Object per org.

import { dropReason } from "./bots.mjs";
import { MAX_BATCH, MAX_BODY_BYTES, parseOne } from "./payload.mjs";
import { TRACKER_JS } from "./tracker.mjs";
import { dailySalt, visitorHash } from "./visitor.mjs";
import { buildReport, sendReport } from "./report.mjs";

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
    const visitor = await visitorHash({ salt, siteKey: key, ip, userAgent: ua });
    const ts = new Date(now).toISOString();
    const out = events.map((e) => ({ ...e, visitor, country, ts }));
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
    if (request.method === "GET" && url.pathname === "/api/stats") return owner(request, env, url);
    if (request.method === "POST" && url.pathname === "/api/report") return owner(request, env, url);
    if (request.method === "POST" && url.pathname === "/api/send") return collect(request, env, false);
    if (request.method === "POST" && url.pathname === "/api/batch") return collect(request, env, true);
    return reply(404, { error: "not found" });
  },
};
