// Session replay ingestion (2026-10-15): POST /api/replay, one chunk of
// rrweb events at a time. Mirrors the main collector's shape (bot/DNT drop,
// site-key + hostname check, hard size caps) but writes to R2 (events) and
// D1 (session metadata) instead of the events table, and is gated behind the
// site's paid plan — replay is a paid-tier feature, never free-tier.
//
// Every cap here is enforced again on the server: a tampered or buggy client
// cannot record longer, bigger or more sessions than the site owner allowed.

import { dropReason } from "../bots.mjs";
import { hostMatches } from "../selfserve.mjs";
import { resolveReplayConfig } from "./config.mjs";
import { chunkKey, getSession, sessionsThisMonth, sessionExists, upsertSessionChunk } from "./store.mjs";

export const REPLAY_MAX_BODY_BYTES = 300_000;
export const REPLAY_MAX_EVENTS_PER_CHUNK = 500;
const SESSION_ID = /^[A-Za-z0-9_-]{8,80}$/;

// The tag is embedded on third-party sites, exactly like /api/send; the response must carry
// the same CORS header or the browser never lets the loader's own fetch() read it.
const CORS = { "access-control-allow-origin": "*" };
const json = (body, status, extra = {}) => Response.json(body, { status, headers: { ...CORS, ...extra } });

/** @returns {null | "bot" | "dnt" | "gpc"} why the request is silently dropped, or null to keep it. */
function privacyDrop(headers) {
  const bot = dropReason(headers);
  if (bot) return bot;
  if (headers.get("dnt") === "1") return "dnt";
  if (headers.get("sec-gpc") === "1") return "gpc";
  return null;
}

function badRequest(error) {
  return json({ error }, 400);
}

/**
 * Handles one POST /api/replay chunk.
 * @param {(key: string, now: number) => Promise<object|null>} lookupSite same cache the main collector uses.
 */
export async function ingestReplayChunk(request, env, now, lookupSite) {
  const drop = privacyDrop(request.headers);
  if (drop) return json({ ok: true }, 202, { "x-railyard-dropped": drop });
  if (!env.DB || !env.REPLAY) return json({ error: "replay storage not configured" }, 503);
  const contentLength = Number(request.headers.get("content-length")) || 0;
  if (contentLength > REPLAY_MAX_BODY_BYTES) return json({ error: "too large" }, 413);
  const text = await request.text();
  if (text.length > REPLAY_MAX_BODY_BYTES) return json({ error: "too large" }, 413);
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    return badRequest("invalid json");
  }
  const siteKey = typeof body?.site === "string" ? body.site : null;
  const sessionId = typeof body?.session === "string" && SESSION_ID.test(body.session) ? body.session : null;
  const seq = Number.isInteger(body?.seq) && body.seq >= 0 ? body.seq : null;
  const events = Array.isArray(body?.events) ? body.events : null;
  if (!siteKey || !sessionId || seq === null) return badRequest("missing site, session or seq");
  if (!events || !events.length || events.length > REPLAY_MAX_EVENTS_PER_CHUNK) return badRequest(`send 1 to ${REPLAY_MAX_EVENTS_PER_CHUNK} events`);

  const site = await lookupSite(siteKey, now);
  if (!site) return json({ error: "unknown site" }, 404);
  if (site.paused) return json({ ok: true }, 202, { "x-railyard-paused": "1" });
  const cfg = resolveReplayConfig(site);
  // Replay is a paid-tier feature: both the owner's toggle and the site's current paid state must hold.
  if (!cfg.enabled || !site.paid) return json({ ok: true, stop: true }, 202, { "x-railyard-replay-off": "1" });

  const hostname = typeof body.hostname === "string" ? body.hostname.slice(0, 100) : null;
  if (site.hostname && hostname && !hostMatches(hostname, site.hostname)) return json({ error: "hostname mismatch" }, 403);
  const url = typeof body.url === "string" ? body.url.slice(0, 1000) : null;
  const ended = body.ended === true;

  if (seq === 0) {
    if (await sessionExists(env.DB, siteKey, sessionId)) {
      // A retried first chunk (e.g. sendBeacon fired twice): treat as a later chunk instead of double-counting the session.
    } else {
      const usedThisMonth = await sessionsThisMonth(env.DB, siteKey, now);
      if (usedThisMonth >= cfg.sessions_per_month) return json({ ok: true, stop: true }, 202, { "x-railyard-replay-capped": "1" });
    }
  }

  const existing = seq > 0 ? await getSession(env.DB, siteKey, sessionId) : null;
  const priorBytes = existing?.bytes ?? 0;
  const bytesThisChunk = text.length;
  let storeEvents = events;
  let truncated = false;
  if (priorBytes + bytesThisChunk > cfg.max_bytes) {
    // Over the per-session byte cap: drop this chunk's events but still mark the session as truncated and tell the client to stop.
    storeEvents = [];
    truncated = true;
  } else if (existing && existing.duration_ms > cfg.max_session_ms) {
    storeEvents = [];
    truncated = true;
  }

  if (storeEvents.length) {
    const country = request.cf?.country ?? null;
    await env.REPLAY.put(chunkKey(siteKey, sessionId, seq), JSON.stringify(storeEvents), { httpMetadata: { contentType: "application/json" } });
    await upsertSessionChunk(env.DB, {
      siteKey,
      sessionId,
      now,
      url,
      hostname,
      country,
      visitor: null,
      eventCount: storeEvents.length,
      bytes: bytesThisChunk,
      ended,
      truncated,
    });
  } else if (ended || truncated) {
    await upsertSessionChunk(env.DB, { siteKey, sessionId, now, url, hostname, country: request.cf?.country ?? null, visitor: null, eventCount: 0, bytes: 0, ended, truncated });
  }

  return json({ ok: true, stop: truncated }, 202);
}
