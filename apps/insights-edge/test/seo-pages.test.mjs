// SEO landing pages (2026-10-14, named comparisons added 2026-10-15): each
// targets one real search query, linked from the landing page and listed in
// the sitemap. `/cookieless-analytics` and `/google-analytics-alternative`
// make claims only about Railyard Insights, with no competitor comparison
// table. `/plausible-alternative` and `/fathom-analytics-alternative` name a
// specific competitor and compare against numbers taken from that
// competitor's own live pricing page (cited inline on the page) — matching
// the organic "alternative to" pages already shipped elsewhere in this
// portfolio (vcpeer.com's /wetransfer-alternative). This is an organic page,
// never a paid ad.
import assert from "node:assert/strict";
import { test } from "node:test";
import { startCollector } from "./harness.mjs";

const bindings = { INGEST_URL: "" };

const NO_COMPARISON_PAGES = [
  { path: "/cookieless-analytics", must: [/Cookieless website analytics/i, /one-way hash/i, /Do Not Track/i] },
  { path: "/google-analytics-alternative", must: [/Google Analytics alternative/i, /No cookie/i] },
];

const NAMED_COMPARISON_PAGES = [
  { path: "/plausible-alternative", competitor: /Plausible/, must: [/Plausible/, /\$9\/month/, /10,000 monthly pageviews/i, /no card/i] },
  { path: "/fathom-analytics-alternative", competitor: /Fathom/, must: [/Fathom/, /\$15\/month/, /100,000 monthly pageviews/i, /no card/i] },
];

const ALL_PATHS = [...NO_COMPARISON_PAGES, ...NAMED_COMPARISON_PAGES].map((p) => p.path);

test("pages with no named competitor serve 200, true claims only about this product, no comparison table", async () => {
  const c = await startCollector({ bindings });
  try {
    for (const { path, must } of NO_COMPARISON_PAGES) {
      const res = await fetch(`${c.base}${path}`);
      assert.equal(res.status, 200, path);
      assert.match(res.headers.get("content-type"), /text\/html/);
      const html = await res.text();
      for (const re of must) assert.match(html, re, `${path} should match ${re}`);
      assert.doesNotMatch(html, /<table/i, `${path} must not contain a comparison table`);
      assert.match(html, /link rel="canonical"/);
      assert.match(html, /<form class="signup"/);
    }
  } finally {
    await c.close();
  }
});

test("named-competitor alternative pages serve 200, name the competitor, cite a live source, and compare fairly", async () => {
  const c = await startCollector({ bindings });
  try {
    for (const { path, must } of NAMED_COMPARISON_PAGES) {
      const res = await fetch(`${c.base}${path}`);
      assert.equal(res.status, 200, path);
      assert.match(res.headers.get("content-type"), /text\/html/);
      const html = await res.text();
      for (const re of must) assert.match(html, re, `${path} should match ${re}`);
      assert.match(html, /<table/i, `${path} should show a side-by-side comparison table`);
      // Cites where the competitor's numbers came from, with a date, so the claim is checkable.
      assert.match(html, /pricing page, 2026-10-15/, `${path} should cite when the competitor's numbers were checked`);
      // Honest about where the competitor is ahead, not only where Railyard wins.
      assert.match(html, /Where (Plausible|Fathom) is ahead/, `${path} should name a real tradeoff, not just wins`);
      assert.match(html, /link rel="canonical"/);
      assert.match(html, /<form class="signup"/);
    }
  } finally {
    await c.close();
  }
});

test("the landing page links every SEO page, and they are all in the sitemap", async () => {
  const c = await startCollector({ bindings });
  try {
    const home = await (await fetch(`${c.base}/`)).text();
    for (const path of ALL_PATHS) assert.match(home, new RegExp(`href="${path}"`), `landing page should link ${path}`);

    const sitemap = await (await fetch(`${c.base}/sitemap.xml`)).text();
    for (const path of ALL_PATHS) assert.match(sitemap, new RegExp(`<loc>https://insights\\.ereboslab\\.com${path}</loc>`), `sitemap should list ${path}`);
  } finally {
    await c.close();
  }
});
