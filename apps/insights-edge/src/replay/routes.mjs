// HTTP glue for session replay's dashboard-facing routes: the settings +
// session list page, the player page, the events JSON a token's own player
// fetches, and the public (unauthenticated, site-key only) replay config the
// page tag asks for before it records anything. Every one of these resolves
// the dashboard token first — exactly resolveDashboard's existing hashed-token
// lookup the rest of Insights already relies on — so a site owner can only
// ever see sessions that belong to their own site.

import { billingReady } from "../dodo.mjs";
import { messagePage } from "../pages.mjs";
import { resolveDashboard } from "../selfserve.mjs";
import { parseReplaySettings, publicReplayConfig } from "./config.mjs";
import { getSession, listSessions, readSessionEvents } from "./store.mjs";
import { replayPlayerPage, replaySessionsPage } from "./pages.mjs";

const DAY = 86_400_000;
const PRIV_HEADERS = { "referrer-policy": "no-referrer", "x-robots-tag": "noindex, nofollow", "cache-control": "no-store, no-transform" };
const PAGE_HEADERS = {
  "content-type": "text/html; charset=utf-8",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
  "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'self'; connect-src 'self'; img-src 'self' data:; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
};
const html = (status, body, extra = {}) => new Response(body, { status, headers: { ...PAGE_HEADERS, "cache-control": "no-store", ...extra } });
const json = (status, body, extra = {}) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store", ...extra } });
// /api/replay/config is called from the third-party page tag, cross-origin, exactly like /api/send.
const PUBLIC_API = { "access-control-allow-origin": "*" };

/** GET /d/<token>/replay, GET /d/<token>/replay/<sessionId>, POST /d/<token>/replay/settings */
export async function replayDashboardRoute(request, env, url, parts) {
  const token = parts[2];
  const found = env.DB ? await resolveDashboard(env, token) : null;
  if (!found) return html(404, messagePage("Dashboard not found", "This link is not valid, or the site was deleted."), PRIV_HEADERS);
  const sub = parts[4];

  if (sub === "settings") {
    if (request.method !== "POST") return html(405, messagePage("Not allowed", "Use the form on the session replay page."), PRIV_HEADERS);
    if (!found.site.paid) return html(403, messagePage("Upgrade required", "Session replay is a paid-tier feature. Upgrade from the main dashboard first."), PRIV_HEADERS);
    let fields;
    try {
      fields = Object.fromEntries((await request.formData()).entries());
    } catch {
      return html(400, messagePage("Not saved", "Could not read the form."), PRIV_HEADERS);
    }
    const parsed = parseReplaySettings(fields);
    if (!parsed.ok) return html(400, messagePage("Not saved", parsed.error, "error"), PRIV_HEADERS);
    const updated = { ...found.site, replay: { ...(found.site.replay ?? {}), ...parsed.replay } };
    await env.SITES.put(`site:${found.key}`, JSON.stringify(updated));
    return new Response(null, { status: 303, headers: { location: `/d/${token}/replay?saved=1`, "cache-control": "no-store" } });
  }

  if (sub) {
    const session = env.DB ? await getSession(env.DB, found.key, sub) : null;
    if (!session) return html(404, messagePage("Session not found", "This session does not exist, or was deleted."), PRIV_HEADERS);
    return html(200, replayPlayerPage({ token, origin: url.origin, site: found.site, session }), PRIV_HEADERS);
  }

  const days = [7, 30, 90].includes(Number(url.searchParams.get("days"))) ? Number(url.searchParams.get("days")) : 7;
  const since = new Date(Date.now() - days * DAY).toISOString();
  const until = new Date(Date.now() + DAY).toISOString();
  const sessions = env.DB ? await listSessions(env.DB, found.key, since, until) : [];
  return html(
    200,
    replaySessionsPage({
      token,
      origin: url.origin,
      site: found.site,
      days,
      sessions,
      paid: Boolean(found.site.paid),
      billingReady: billingReady(env),
      saved: url.searchParams.get("saved") === "1",
    }),
    PRIV_HEADERS,
  );
}

/** GET /api/d/<token>/replay/<sessionId>/events — the only way to read one session's events back; scoped to the token's own site. */
export async function replayEventsApi(env, token, sessionId) {
  const found = env.DB ? await resolveDashboard(env, token) : null;
  if (!found) return json(404, { error: "not found" }, PRIV_HEADERS);
  const session = await getSession(env.DB, found.key, sessionId);
  if (!session) return json(404, { error: "not found" }, PRIV_HEADERS);
  const events = env.REPLAY ? await readSessionEvents(env.REPLAY, found.key, sessionId) : [];
  return json(200, events ?? [], PRIV_HEADERS);
}

/** GET /api/replay/config?site=<key> — public and unauthenticated, like /s.js: the site key is already visible in the page's own HTML source. Never reveals session content, only the recording knobs. */
export async function replayPublicConfig(siteKey, lookupSite, now) {
  const site = await lookupSite(siteKey, now);
  if (!site) return json(404, { error: "unknown site" }, PUBLIC_API);
  const cfg = publicReplayConfig(site);
  return json(200, { ...cfg, enabled: cfg.enabled && site.paid === true }, PUBLIC_API);
}
