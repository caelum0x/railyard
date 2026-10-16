// Live example page (2026-10-16): a public, read-only view of this site's
// own real numbers -- the same dashboard every Railyard Insights sign-up
// gets -- so a visitor can see the product work before giving an email
// address. No auth, no PII, no customer's site data: only the landing
// site's own aggregate counts (site key LANDING_SITE_KEY).
import assert from "node:assert/strict";
import { test } from "node:test";
import { ev, sleep, startCollector } from "./harness.mjs";

const LANDING = "ry_insights_landing";
const sites = { [LANDING]: { org_id: "org-insights", app_id: "insights-landing", label: "insights.ereboslab.com", events_limit: 100000 } };
const bindings = { INGEST_URL: "", LANDING_SITE_KEY: LANDING };

test("/live-demo works with no DB bound: 200, zero stats, still offers sign-up", async () => {
  const c = await startCollector({ bindings });
  try {
    const res = await fetch(`${c.base}/live-demo`);
    assert.equal(res.status, 200);
    assert.match(res.headers.get("content-type"), /text\/html/);
    const html = await res.text();
    assert.match(html, /live example/i);
    assert.match(html, /<form class="signup"/);
    assert.match(html, /link rel="canonical"/);
  } finally {
    await c.close();
  }
});

test("/live-demo shows this site's own real numbers, no login needed", async () => {
  const c = await startCollector({ d1: true, sites, bindings });
  try {
    for (const url of ["/", "/", "/live-demo"]) assert.equal((await c.send(ev(LANDING, url))).status, 202);
    await sleep(3500);
    const res = await fetch(`${c.base}/live-demo?days=30`);
    assert.equal(res.status, 200);
    const html = await res.text();
    // 3 page views from 1 visitor (same IP/UA in the harness's default event).
    assert.match(html, /<b>3<\/b>page views/);
    assert.match(html, /<b>1<\/b>visitors/);
    assert.match(html, /<table/i, "should list top pages");
  } finally {
    await c.close();
  }
});

test("the landing page links /live-demo, and it is in the sitemap", async () => {
  const c = await startCollector({ bindings });
  try {
    const home = await (await fetch(`${c.base}/`)).text();
    assert.match(home, /href="\/live-demo"/);
    const sitemap = await (await fetch(`${c.base}/sitemap.xml`)).text();
    assert.match(sitemap, /<loc>https:\/\/insights\.ereboslab\.com\/live-demo<\/loc>/);
  } finally {
    await c.close();
  }
});
