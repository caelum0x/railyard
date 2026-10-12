// Self-serve sign-up (2026-10-12): anyone adds a site with an email and a
// domain, gets the page tag and a private dashboard link at once, sees their
// numbers there, and can delete the site and every stored event.
import assert from "node:assert/strict";
import { test } from "node:test";
import { ev, sleep, startCollector } from "./harness.mjs";

const OWN = "ry_site_owned001";
const sites = { [OWN]: { org_id: "org-1", app_id: "bidstage", label: "bidstage.app", events_limit: 100000 } };
const bindings = { INGEST_URL: "", STATS_TOKEN: "stats-token-123", RALLY_TX_URL: "https://mail.test/send", RALLY_TX_KEY: "smail_test", REPORT_TO: "owner@example.com" };
const mailOk = (e) => (e.url.startsWith("https://mail.test") ? 202 : 200);

const signup = (c, body, ip = "198.51.100.9") =>
  fetch(`${c.base}/api/sites`, { method: "POST", headers: { "content-type": "application/json", "cf-connecting-ip": ip }, body: JSON.stringify(body) });

test("the landing page, privacy page, robots and sitemap are served", async () => {
  const c = await startCollector({ d1: true, sites, bindings });
  try {
    const home = await fetch(`${c.base}/`);
    assert.equal(home.status, 200);
    assert.match(home.headers.get("content-type"), /text\/html/);
    const html = await home.text();
    assert.match(html, /<form/);
    assert.match(html, /10,000 events/);
    assert.match(html, /href="\/privacy"/);
    const privacy = await fetch(`${c.base}/privacy`);
    assert.equal(privacy.status, 200);
    assert.match(await privacy.text(), /delete/i);
    assert.equal((await fetch(`${c.base}/robots.txt`)).status, 200);
    assert.match(await (await fetch(`${c.base}/sitemap.xml`)).text(), /<urlset/);
  } finally {
    await c.close();
  }
});

test("sign-up validates the email and the domain", async () => {
  const c = await startCollector({ d1: true, sites, bindings, respond: mailOk });
  try {
    for (const body of [{ email: "nope", domain: "shop.example" }, { email: "a@b.co", domain: "not a domain" }, { email: "a@b.co", domain: "" }, {}]) {
      const res = await signup(c, body);
      assert.equal(res.status, 400, JSON.stringify(body));
      assert.ok((await res.json()).error);
    }
  } finally {
    await c.close();
  }
});

test("sign-up gives a tag and dashboard; events show there; foreign hostnames are dropped; delete removes everything", async () => {
  const c = await startCollector({ d1: true, sites, bindings, respond: mailOk });
  try {
    const res = await signup(c, { email: "Founder@Shop.Example", domain: "https://www.shop.example/pricing" });
    assert.equal(res.status, 201);
    const s = await res.json();
    assert.match(s.site_key, /^s_[A-Za-z0-9_-]{20,}$/);
    assert.equal(s.domain, "shop.example");
    assert.match(s.snippet, new RegExp(`data-site="${s.site_key}"`));
    assert.match(s.dashboard_url, /\/d\/[A-Za-z0-9_-]{30,}$/);
    // The copy of the link went out by mail.
    const mail = c.outbound.filter((o) => o.url === "https://mail.test/send");
    assert.equal(mail.length, 1);
    const m = JSON.parse(mail[0].body);
    assert.equal(m.to, "founder@shop.example");
    assert.ok(m.html.includes(s.dashboard_url));

    // Own hostname and a subdomain are stored; someone else's hostname is not.
    assert.equal((await c.send(ev(s.site_key, "/", { hostname: "shop.example" }))).status, 202);
    assert.equal((await c.send(ev(s.site_key, "/?utm_source=newsletter&utm_campaign=oct", { hostname: "www.shop.example" }))).status, 202);
    assert.equal((await c.send(ev(s.site_key, "/", { hostname: "evil.example" }))).status, 202);
    // Page speed from real visits, shown on the dashboard as the 75th percentile.
    for (const [lcp, inp, cls] of [[1000, 40, 0.01], [2000, 120, 0.05], [3000, 300, 0.2], [4000, 500, 0.3]]) {
      assert.equal((await c.send({ type: "performance", payload: { ...ev(s.site_key, "/").payload, lcp, inp, cls } })).status, 202);
    }
    await sleep(3500);
    const db = await c.mf.getD1Database("DB");
    const rows = (await db.prepare("SELECT hostname FROM events WHERE site_key = ?").bind(s.site_key).all()).results;
    assert.deepEqual(rows.map((r) => r.hostname).sort(), ["shop.example", "shop.example", "shop.example", "shop.example", "shop.example", "www.shop.example"]);

    const token = s.dashboard_url.split("/d/")[1];
    const stats = await (await fetch(`${c.base}/api/d/${token}/stats?days=1`)).json();
    assert.equal(stats.domain, "shop.example");
    assert.equal(stats.cur.pageviews, 2);
    assert.deepEqual(stats.cur.utm_sources, [{ source: "newsletter", n: 1 }]);
    assert.deepEqual(stats.speed, { samples: 4, lcp: 3000, inp: 300, cls: 0.2 });
    const page = await fetch(`${c.base}/d/${token}`);
    assert.equal(page.status, 200);
    assert.equal(page.headers.get("referrer-policy"), "no-referrer");
    const html = await page.text();
    assert.match(html, /shop\.example/);
    assert.match(html, /newsletter/);
    assert.match(html, /Page speed/);
    assert.match(html, /3\.0 s/);
    assert.equal((await fetch(`${c.base}/d/not-a-real-token-aaaaaaaaaaaaaaaaaaaaaaaaaa`)).status, 404);

    // The owner's weekly report leaves self-serve sites out.
    const owner = await (await fetch(`${c.base}/api/stats?days=1`, { headers: { authorization: "Bearer stats-token-123" } })).json();
    assert.deepEqual(owner.rows.map((r) => r.site), ["bidstage.app"]);
    assert.equal(owner.self_serve_sites, 1);

    const del = await fetch(`${c.base}/d/${token}/delete`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "confirm=shop.example" });
    assert.equal(del.status, 200);
    assert.equal((await db.prepare("SELECT COUNT(*) AS n FROM events WHERE site_key = ?").bind(s.site_key).first()).n, 0);
    assert.equal((await fetch(`${c.base}/d/${token}`)).status, 404);
    const kv = await c.mf.getKVNamespace("SITES");
    assert.equal(await kv.get(`site:${s.site_key}`), null);
  } finally {
    await c.close();
  }
});

test("delete needs the domain typed back", async () => {
  const c = await startCollector({ d1: true, sites, bindings, respond: mailOk });
  try {
    const s = await (await signup(c, { email: "a@shop.example", domain: "shop.example" })).json();
    const token = s.dashboard_url.split("/d/")[1];
    const del = await fetch(`${c.base}/d/${token}/delete`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "confirm=wrong" });
    assert.equal(del.status, 400);
    assert.equal((await fetch(`${c.base}/d/${token}`)).status, 200);
  } finally {
    await c.close();
  }
});

test("sign-ups are rate limited per email", async () => {
  const c = await startCollector({ d1: true, sites, bindings, respond: mailOk });
  try {
    for (let i = 0; i < 3; i += 1) assert.equal((await signup(c, { email: "a@b.co", domain: `s${i}.example` })).status, 201);
    assert.equal((await signup(c, { email: "a@b.co", domain: "s9.example" })).status, 429);
  } finally {
    await c.close();
  }
});

test("the no-JavaScript form redirects to the new dashboard, and shows errors on the landing page", async () => {
  const c = await startCollector({ d1: true, sites, bindings, respond: mailOk });
  try {
    const form = (body) => fetch(`${c.base}/api/sites`, { method: "POST", redirect: "manual", headers: { "content-type": "application/x-www-form-urlencoded" }, body });
    const ok = await form("email=a%40shop.example&domain=shop.example");
    assert.equal(ok.status, 303);
    assert.match(ok.headers.get("location"), /^\/d\/[A-Za-z0-9_-]{30,}\?new=mailed$/);
    const page = await (await fetch(`${c.base}${ok.headers.get("location")}`)).text();
    assert.match(page, /Step 1: add the tag/);
    assert.match(page, /emailed you the link/);
    const bad = await form("email=nope&domain=%3Cscript%3E");
    assert.equal(bad.status, 400);
    const html = await bad.text();
    assert.match(html, /Type a full email address/);
    assert.ok(!html.includes("<script>"), "input is escaped");
  } finally {
    await c.close();
  }
});
