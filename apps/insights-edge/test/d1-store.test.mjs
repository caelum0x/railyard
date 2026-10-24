// The $0 sink: events land in D1, the stats endpoint reads them back behind
// STATS_TOKEN, and the report endpoint sends one Rally Send email.
import assert from "node:assert/strict";
import { test } from "node:test";
import { ev, sleep, startCollector } from "./harness.mjs";

const SITE = "ry_site_d1test01";
const sites = { [SITE]: { org_id: "org-1", app_id: "bidstage", label: "bidstage.app", events_limit: 100000 } };
const bindings = { INGEST_URL: "", STATS_TOKEN: "stats-token-123", RALLY_TX_URL: "https://mail.test/send", RALLY_TX_KEY: "smail_test", REPORT_TO: "owner@example.com" };

test("page views and custom events are stored in D1 and summarised", async () => {
  const c = await startCollector({ d1: true, sites, bindings });
  try {
    for (const url of ["/", "/pricing"]) assert.equal((await c.send(ev(SITE, url, { referrer: "https://news.example/a" }))).status, 202);
    assert.equal((await c.send(ev(SITE, "/pricing", { name: "signup" }))).status, 202);
    await sleep(3500);
    const db = await c.mf.getD1Database("DB");
    const rows = (await db.prepare("SELECT * FROM events ORDER BY id").all()).results;
    assert.equal(rows.length, 3);
    for (const r of rows) {
      assert.equal(r.site_key, SITE);
      assert.match(r.visitor, /^[0-9a-f]{32}$/);
      assert.ok(!JSON.stringify(r).includes("203.0.113.77"), "no IP stored");
    }
    assert.equal((await fetch(`${c.base}/api/stats`)).status, 401);
    const stats = await (await fetch(`${c.base}/api/stats?days=1`, { headers: { authorization: "Bearer stats-token-123" } })).json();
    const s = stats.rows[0];
    assert.equal(s.site, "bidstage.app");
    assert.equal(s.cur.pageviews, 2);
    assert.equal(s.cur.visitors, 1);
    assert.equal(s.cur.engaged, 1);
    assert.equal(s.cur.converted, 1);
    assert.deepEqual(s.cur.top_referrers, [{ host: "news.example", n: 2 }]);
    assert.deepEqual(s.cur.events, [{ name: "signup", n: 1 }]);
    assert.ok(!c.outbound.some((o) => o.url.startsWith("https://ingest.test")), "no ingest forward without INGEST_URL");
  } finally {
    await c.close();
  }
});

test("the report endpoint sends one Rally Send email to the owner", async () => {
  const c = await startCollector({ d1: true, sites, bindings, respond: (e) => (e.url.startsWith("https://mail.test") ? 202 : 200) });
  try {
    const res = await fetch(`${c.base}/api/report`, { method: "POST", headers: { authorization: "Bearer stats-token-123" } });
    const body = await res.json();
    assert.equal(body.sent, true);
    const mail = c.outbound.filter((o) => o.url === "https://mail.test/send");
    assert.equal(mail.length, 1);
    const m = JSON.parse(mail[0].body);
    assert.equal(m.to, "owner@example.com");
    assert.equal(m.product, "menivor");
    assert.match(m.subject, /^Railyard Insights week/);
    assert.match(m.html, /bidstage\.app/);
    assert.equal(mail[0].auth, "Bearer smail_test");
  } finally {
    await c.close();
  }
});

test("the funnel counts same-day visitor-days that progress through ordered steps, via the real API", async () => {
  const c = await startCollector({ d1: true, sites, bindings });
  try {
    // 2 visitors (distinguished by IP, like real distinct visitors) reach "/",
    // only 1 of them goes on to "/pricing", and only that one then fires the
    // "signup" custom event -- a real 3-step funnel, not a guess.
    await c.send(ev(SITE, "/"), { "cf-connecting-ip": "203.0.113.11" });
    await c.send(ev(SITE, "/pricing"), { "cf-connecting-ip": "203.0.113.11" });
    await c.send(ev(SITE, "/pricing", { name: "signup" }), { "cf-connecting-ip": "203.0.113.11" });
    await c.send(ev(SITE, "/"), { "cf-connecting-ip": "203.0.113.12" });
    await sleep(3500);
    const res = await fetch(`${c.base}/api/funnel?site=${SITE}&days=1&steps=/,/pricing*`, { headers: { authorization: "Bearer stats-token-123" } });
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.equal(body.steps.length, 2);
    assert.equal(body.steps[0].step, "/");
    assert.equal(body.steps[0].visitors, 2);
    assert.equal(body.steps[1].step, "/pricing*");
    assert.equal(body.steps[1].visitors, 1);
    assert.equal(body.steps[1].rate, 50);
    assert.equal((await fetch(`${c.base}/api/funnel?site=${SITE}&days=1`)).status, 401);
  } finally {
    await c.close();
  }
});
