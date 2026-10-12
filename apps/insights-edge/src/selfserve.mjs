// Self-serve sign-up (2026-10-12). Anyone adds a site with an email and a
// domain and gets, at once, the page tag and a private dashboard link (also
// mailed to them). The link's token is stored only as a SHA-256 hash. A site
// stores events only from its own domain and subdomains, keeps the free cap,
// and can be deleted with every stored event from its dashboard.

import { ensureSchema } from "./store.mjs";

export const FREE_EVENTS_PER_MONTH = 10_000;
export const PER_EMAIL_PER_DAY = 3;
export const PER_DOMAIN_PER_DAY = 3;
export const DEFAULT_DAILY_CAP = 300;
const DAY = 86_400_000;

const SIGNUP_SCHEMA = [
  `CREATE TABLE IF NOT EXISTS signups (
    site_key TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    domain TEXT NOT NULL,
    dash_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
  )`,
  "CREATE INDEX IF NOT EXISTS signups_email ON signups (email, created_at)",
  "CREATE INDEX IF NOT EXISTS signups_domain ON signups (domain, created_at)",
  "CREATE INDEX IF NOT EXISTS signups_created ON signups (created_at)",
];

let signupReady = null;
function ensureSignups(db) {
  if (!signupReady) signupReady = db.batch(SIGNUP_SCHEMA.map((s) => db.prepare(s))).catch((err) => {
    signupReady = null;
    throw err;
  });
  return signupReady;
}

const EMAIL = /^[^\s@<>"']{1,64}@[a-z0-9.-]+\.[a-z]{2,}$/;
const HOST = /^(?=.{4,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z][a-z0-9-]{0,61}[a-z0-9]$/;
const TOKEN = /^[A-Za-z0-9_-]{30,80}$/;

export function normalizeEmail(v) {
  const e = typeof v === "string" ? v.trim().toLowerCase() : "";
  return e.length <= 254 && EMAIL.test(e) ? e : null;
}

/** `https://www.Shop.example:443/pricing` -> `shop.example`; null when it is not a public hostname. */
export function normalizeDomain(v) {
  if (typeof v !== "string") return null;
  let h = v.trim().toLowerCase();
  if (!h) return null;
  h = h.replace(/^[a-z]+:\/\//, "").split(/[/?#]/)[0].replace(/:\d+$/, "").replace(/\.$/, "").replace(/^www\./, "");
  return HOST.test(h) ? h : null;
}

/** An event's hostname belongs to the site's domain (or a subdomain of it). */
export function hostMatches(eventHost, domain) {
  if (!domain) return true;
  const h = String(eventHost ?? "").toLowerCase().replace(/\.$/, "");
  return h === domain || h.endsWith(`.${domain}`);
}

function randomId(bytes) {
  const b = crypto.getRandomValues(new Uint8Array(bytes));
  return btoa(String.fromCharCode(...b)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export async function sha256Hex(text) {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(d)].map((x) => x.toString(16).padStart(2, "0")).join("");
}

export const snippetFor = (origin, key) => `<script defer src="${origin}/s.js" data-site="${key}"></script>`;

async function limited(db, email, domain, now, dailyCap) {
  const since = new Date(now - DAY).toISOString();
  const [byEmail, byDomain, total] = await db.batch([
    db.prepare("SELECT COUNT(*) AS n FROM signups WHERE email = ? AND created_at >= ?").bind(email, since),
    db.prepare("SELECT COUNT(*) AS n FROM signups WHERE domain = ? AND created_at >= ?").bind(domain, since),
    db.prepare("SELECT COUNT(*) AS n FROM signups WHERE created_at >= ?").bind(since),
  ]);
  return byEmail.results[0].n >= PER_EMAIL_PER_DAY || byDomain.results[0].n >= PER_DOMAIN_PER_DAY || total.results[0].n >= dailyCap;
}

/**
 * @returns {Promise<{status: 201, body: object} | {status: 400 | 429 | 503, body: {error: string}}>}
 */
export async function createSite(env, input, origin, now = Date.now(), fetcher = fetch) {
  if (!env.DB || !env.SITES) return { status: 503, body: { error: "Sign-up is not available right now." } };
  const email = normalizeEmail(input?.email);
  if (!email) return { status: 400, body: { error: "Type a full email address." } };
  const domain = normalizeDomain(input?.domain);
  if (!domain) return { status: 400, body: { error: "Type your site's domain, like shop.example.com." } };
  await ensureSignups(env.DB);
  const cap = Number(env.SIGNUP_DAILY_CAP) || DEFAULT_DAILY_CAP;
  if (await limited(env.DB, email, domain, now, cap)) return { status: 429, body: { error: "Too many sign-ups right now. Try again tomorrow." } };

  const key = `s_${randomId(18)}`;
  const token = randomId(32);
  const dashHash = await sha256Hex(token);
  const createdAt = new Date(now).toISOString();
  const entry = { org_id: `self-${key}`, app_id: key, label: domain, hostname: domain, events_limit: FREE_EVENTS_PER_MONTH, self_serve: true, dash_hash: dashHash, created_at: createdAt };
  await env.DB.prepare("INSERT INTO signups (site_key, email, domain, dash_hash, created_at) VALUES (?, ?, ?, ?, ?)").bind(key, email, domain, dashHash, createdAt).run();
  await env.SITES.put(`dash:${dashHash}`, key);
  await env.SITES.put(`site:${key}`, JSON.stringify(entry), { metadata: { self_serve: true } });

  const dashboardUrl = `${origin}/d/${token}`;
  const snippet = snippetFor(origin, key);
  const mailed = await mailLink(env, { email, domain, snippet, dashboardUrl }, fetcher);
  return { status: 201, body: { site_key: key, domain, snippet, dashboard_url: dashboardUrl, mailed } };
}

const esc = (v) => String(v).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

/** Best effort: the sign-up already succeeded and the page shows the same link. */
async function mailLink(env, { email, domain, snippet, dashboardUrl }, fetcher) {
  if (!env.RALLY_TX_URL || !env.RALLY_TX_KEY) return false;
  const subject = `Your analytics tag for ${domain}`;
  const html = `<div style="font-family:system-ui,sans-serif;font-size:14px;color:#111">
<p>Here is the cookieless analytics tag for <b>${esc(domain)}</b>. Paste it into the <code>&lt;head&gt;</code> of every page:</p>
<pre style="background:#f4f4f4;padding:10px;white-space:pre-wrap">${esc(snippet)}</pre>
<p>Your private dashboard (keep this link to yourself, anyone with it can see the numbers):<br><a href="${esc(dashboardUrl)}">${esc(dashboardUrl)}</a></p>
<p style="color:#666;font-size:12px">You get this because this address was used to add ${esc(domain)} to Railyard Insights. If that was not you, open the link and delete the site.</p></div>`;
  const text = `Cookieless analytics tag for ${domain}. Paste into the <head> of every page:\n\n${snippet}\n\nPrivate dashboard (keep it to yourself):\n${dashboardUrl}\n\nIf you did not ask for this, open the link and delete the site.`;
  try {
    const res = await fetcher(env.RALLY_TX_URL, {
      method: "POST",
      headers: { authorization: `Bearer ${env.RALLY_TX_KEY}`, "content-type": "application/json" },
      body: JSON.stringify({
        product: env.REPORT_PRODUCT ?? "menivor",
        project: "railyard-insights",
        to: email,
        from_local: env.REPORT_FROM_LOCAL ?? "no-reply",
        from_name: "Railyard Insights",
        subject,
        html,
        text,
        idempotency_key: `railyard-insights-signup-${dashboardUrl.slice(-16)}`,
        tags: ["railyard-insights-signup"],
      }),
    });
    if (res.status !== 202) console.error(`insights-edge: sign-up mail answered ${res.status}`);
    return res.status === 202;
  } catch (err) {
    console.error(`insights-edge: sign-up mail failed: ${err.message}`);
    return false;
  }
}

/** The site behind a dashboard token, or null. */
export async function resolveDashboard(env, token) {
  if (!env.SITES || typeof token !== "string" || !TOKEN.test(token)) return null;
  const key = await env.SITES.get(`dash:${await sha256Hex(token)}`);
  if (!key) return null;
  const raw = await env.SITES.get(`site:${key}`);
  return raw ? { key, site: JSON.parse(raw) } : null;
}

/** Stored events this UTC month and all time, for the cap line and the install check. */
export async function usage(db, key, now = Date.now()) {
  await ensureSchema(db);
  const month = `${new Date(now).toISOString().slice(0, 7)}-01T00:00:00.000Z`;
  const [m, all] = await db.batch([
    db.prepare("SELECT COUNT(*) AS n FROM events WHERE site_key = ? AND ts >= ?").bind(key, month),
    db.prepare("SELECT COUNT(*) AS n FROM events WHERE site_key = ?").bind(key),
  ]);
  return { month: m.results[0].n, all: all.results[0].n };
}

/** Removes the site, its dashboard link, its sign-up row and every stored event. */
export async function deleteSite(env, key, site) {
  await ensureSchema(env.DB);
  await ensureSignups(env.DB);
  await env.DB.batch([env.DB.prepare("DELETE FROM events WHERE site_key = ?").bind(key), env.DB.prepare("DELETE FROM signups WHERE site_key = ?").bind(key)]);
  await env.SITES.delete(`site:${key}`);
  if (site.dash_hash) await env.SITES.delete(`dash:${site.dash_hash}`);
}
