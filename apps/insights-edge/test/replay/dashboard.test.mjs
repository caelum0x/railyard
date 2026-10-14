// Session replay dashboard: the settings + session list page, the player
// page, and the events API — all behind the same private dashboard token as
// the rest of Insights. The one property this file exists to prove: a site
// owner can see and play back only their own site's sessions, never another
// site's, even knowing its exact session id.
import assert from "node:assert/strict";
import { test } from "node:test";
import { startCollector } from "../harness.mjs";

const bindings = { INGEST_URL: "", STATS_TOKEN: "stats-token-123" };
const REAL_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Safari/605.1.15";

const signup = (c, body) => fetch(`${c.base}/api/sites`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) }).then((r) => r.json());
const tokenOf = (s) => s.dashboard_url.split("/d/")[1];

async function markPaid(c, siteKey) {
  const kv = await c.mf.getKVNamespace("SITES");
  const site = JSON.parse(await kv.get(`site:${siteKey}`));
  await kv.put(`site:${siteKey}`, JSON.stringify({ ...site, paid: true }));
}

const ingest = (c, siteKey, session, seq = 0, extra = {}) =>
  fetch(`${c.base}/api/replay`, {
    method: "POST",
    headers: { "content-type": "text/plain", "user-agent": REAL_UA },
    body: JSON.stringify({ site: siteKey, session, seq, url: "/pricing", hostname: "shop.example", events: [{ type: 3, data: {}, timestamp: Date.now() }], ...extra }),
  });

test("the settings form is refused until the site is paid, then persists and shows up in the public config and the page", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings });
  try {
    const s = await signup(c, { email: "a@shop.example", domain: "shop.example" });
    const token = tokenOf(s);

    const before = await fetch(`${c.base}/d/${token}/replay/settings`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "enabled=on" });
    assert.equal(before.status, 403);

    await markPaid(c, s.site_key);
    const saved = await fetch(`${c.base}/d/${token}/replay/settings`, {
      method: "POST",
      redirect: "manual",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      // mask_all_inputs=off is what the real settings form sends when its checkbox is unchecked (a hidden
      // fallback field ahead of the checkbox in the form, so Object.fromEntries keeps the checkbox's value
      // only when it is actually checked) — sent explicitly here since this test posts a raw body, not the form.
      body: "enabled=on&sample_rate=0.5&mask_text_selector=.secret&mask_all_inputs=off",
    });
    assert.equal(saved.status, 303);
    assert.equal(saved.headers.get("location"), `/d/${token}/replay?saved=1`);

    const page = await fetch(`${c.base}${saved.headers.get("location")}`);
    const html = await page.text();
    assert.match(html, /Settings saved/);
    assert.match(html, /name="enabled" checked/);
    assert.match(html, /value="0\.5"/);
    assert.match(html, /value="\.secret"/);
    assert.doesNotMatch(html, /name="mask_all_inputs" value="on" checked/, "mask_all_inputs=off must not leave the checkbox checked");

    const cfg = await (await fetch(`${c.base}/api/replay/config?site=${s.site_key}`)).json();
    assert.equal(cfg.enabled, true);
    assert.equal(cfg.sample_rate, 0.5);
    assert.equal(cfg.mask_all_inputs, false);
    assert.equal(cfg.mask_text_selector, ".secret");
  } finally {
    await c.close();
  }
});

test("a recorded session shows on the list, plays back on its own page, and a bad token is refused everywhere", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings });
  try {
    const s = await signup(c, { email: "a@shop.example", domain: "shop.example" });
    const token = tokenOf(s);
    await markPaid(c, s.site_key);
    await fetch(`${c.base}/d/${token}/replay/settings`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "enabled=on&sample_rate=1" });

    const sessionId = "replaysession01";
    assert.equal((await ingest(c, s.site_key, sessionId)).status, 202);

    const list = await fetch(`${c.base}/d/${token}/replay`);
    assert.equal(list.status, 200);
    const listHtml = await list.text();
    assert.match(listHtml, new RegExp(`/d/${token}/replay/${sessionId}`));
    assert.match(listHtml, /\/pricing/);

    const player = await fetch(`${c.base}/d/${token}/replay/${sessionId}`);
    assert.equal(player.status, 200);
    const playerHtml = await player.text();
    assert.match(playerHtml, /replay-player\.js/);
    assert.match(playerHtml, new RegExp(`data-events-url="/api/d/${token}/replay/${sessionId}/events"`));

    const events = await (await fetch(`${c.base}/api/d/${token}/replay/${sessionId}/events`)).json();
    assert.equal(events.length, 1);
    assert.equal(events[0].type, 3);

    const bad = "not-a-real-token-aaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert.equal((await fetch(`${c.base}/d/${bad}/replay`)).status, 404);
    assert.equal((await fetch(`${c.base}/d/${bad}/replay/${sessionId}`)).status, 404);
    assert.equal((await fetch(`${c.base}/api/d/${bad}/replay/${sessionId}/events`)).status, 404);
    assert.equal(
      (await fetch(`${c.base}/d/${bad}/replay/settings`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "enabled=on" })).status,
      404,
      "an unresolvable token 404s, same as every other dashboard route, before the paid-gate check ever runs",
    );
  } finally {
    await c.close();
  }
});

test("a site owner can only see their own sessions: a second site's token cannot read the first site's session, even knowing its exact id", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings });
  try {
    const a = await signup(c, { email: "a@shop.example", domain: "shop.example" });
    const b = await signup(c, { email: "b@other.example", domain: "other.example" });
    await markPaid(c, a.site_key);
    await markPaid(c, b.site_key);
    const tokenA = tokenOf(a);
    const tokenB = tokenOf(b);
    await fetch(`${c.base}/d/${tokenA}/replay/settings`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "enabled=on&sample_rate=1" });

    const sharedSessionId = "sharedsessionid1";
    assert.equal((await ingest(c, a.site_key, sharedSessionId)).status, 202);

    // Site A's own token sees it.
    assert.equal((await fetch(`${c.base}/d/${tokenA}/replay/${sharedSessionId}`)).status, 200);
    assert.equal((await (await fetch(`${c.base}/api/d/${tokenA}/replay/${sharedSessionId}/events`)).json()).length, 1);

    // Site B's token, naming the exact same session id, gets nothing.
    assert.equal((await fetch(`${c.base}/d/${tokenB}/replay/${sharedSessionId}`)).status, 404);
    const crossEvents = await fetch(`${c.base}/api/d/${tokenB}/replay/${sharedSessionId}/events`);
    assert.equal(crossEvents.status, 404);

    // Site B's own session list never mentions site A's session.
    const bList = await (await fetch(`${c.base}/d/${tokenB}/replay`)).text();
    assert.doesNotMatch(bList, new RegExp(sharedSessionId));
  } finally {
    await c.close();
  }
});

test("deleting a site removes its replay sessions (R2 objects and the D1 row), not only its analytics events", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings });
  try {
    const s = await signup(c, { email: "a@shop.example", domain: "shop.example" });
    const token = tokenOf(s);
    await markPaid(c, s.site_key);
    await fetch(`${c.base}/d/${token}/replay/settings`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "enabled=on&sample_rate=1" });
    const sessionId = "deleteme0000001";
    assert.equal((await ingest(c, s.site_key, sessionId)).status, 202);

    const bucket = await c.mf.getR2Bucket("REPLAY");
    assert.equal((await bucket.list({ prefix: `${s.site_key}/` })).objects.length, 1);

    const del = await fetch(`${c.base}/d/${token}/delete`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" }, body: "confirm=shop.example" });
    assert.equal(del.status, 200);

    assert.equal((await bucket.list({ prefix: `${s.site_key}/` })).objects.length, 0);
    const db = await c.mf.getD1Database("DB");
    const row = await db.prepare("SELECT COUNT(*) AS n FROM replay_sessions WHERE site_key = ?").bind(s.site_key).first();
    assert.equal(row.n, 0);
  } finally {
    await c.close();
  }
});
