// SEO landing pages (2026-10-14): three pages, each targeting one real
// search query, linked from the landing page and listed in the sitemap.
// Every claim on them is about Railyard Insights only — no competitor
// comparison table, no claim about anyone else's product.
import assert from "node:assert/strict";
import { test } from "node:test";
import { startCollector } from "./harness.mjs";

const bindings = { INGEST_URL: "" };

const PAGES = [
  { path: "/cookieless-analytics", must: [/Cookieless website analytics/i, /one-way hash/i, /Do Not Track/i] },
  { path: "/google-analytics-alternative", must: [/Google Analytics alternative/i, /No cookie/i] },
  { path: "/plausible-alternative", must: [/free.{0,40}analytics alternative/i, /no card/i] },
];

test("each SEO page serves 200, true claims only about this product, and no comparison table", async () => {
  const c = await startCollector({ bindings });
  try {
    for (const { path, must } of PAGES) {
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

test("the landing page links all three SEO pages, and they are all in the sitemap", async () => {
  const c = await startCollector({ bindings });
  try {
    const home = await (await fetch(`${c.base}/`)).text();
    for (const { path } of PAGES) assert.match(home, new RegExp(`href="${path}"`), `landing page should link ${path}`);

    const sitemap = await (await fetch(`${c.base}/sitemap.xml`)).text();
    for (const { path } of PAGES) assert.match(sitemap, new RegExp(`<loc>https://insights\\.ereboslab\\.com${path}</loc>`), `sitemap should list ${path}`);
  } finally {
    await c.close();
  }
});
