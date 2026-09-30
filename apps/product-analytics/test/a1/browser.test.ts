// One real Chromium run (the cached headless shell of playwright-core 1.63,
// nothing installed): the page loads @railyard/analytics-js with the
// unmodified browser SDK. Before consent the page makes no request to the
// analytics host and stores nothing; after consent the page view, a custom
// event and an autocaptured click are stored; typed input never leaves.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { AddressInfo } from "node:net";
import { chromium } from "playwright-core";
import { makeSite, migratedPg, skipNoPg, sleep, startService, type Running, type ScratchPg } from "./helpers.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const PKG = join(HERE, "../../../../packages/analytics-js");
const SDK_ESM = join(PKG, "node_modules/posthog-js/dist/module.mjs");
const haveBrowser = existsSync(chromium.executablePath()) || existsSync(chromium.executablePath().replace("chromium-", "chromium_headless_shell-"));

let pg: ScratchPg;
let run: Running;
let pageBase = "";
const site = makeSite();

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site]);
  const page = `<!doctype html><html><head><title>Shop</title>
<script type="importmap">{"imports":{"posthog-js":"/vendor/sdk.mjs"}}</script></head>
<body>
  <button id="allow">Allow analytics</button>
  <button id="buy" class="btn">Buy now</button>
  <input id="email" type="email">
  <p data-private>private note</p>
  <script type="module">
    import { createAnalytics } from "/pkg/index.mjs";
    const analytics = createAnalytics({ site: ${JSON.stringify(site.token)}, host: ${JSON.stringify(run.base)} });
    window.analytics = analytics;
    analytics.capture("before_consent");
    document.getElementById("allow").onclick = () => {
      analytics.setConsent("granted");
      analytics.capture("consented", { via: "banner" });
    };
    window.ready = true;
  </script>
</body></html>`;
  const files: Record<string, [string, string]> = {
    "/": [page, "text/html"],
    "/vendor/sdk.mjs": [readFileSync(SDK_ESM, "utf8"), "text/javascript"],
    "/pkg/index.mjs": [readFileSync(join(PKG, "src/index.mjs"), "utf8"), "text/javascript"],
    "/pkg/env.mjs": [readFileSync(join(PKG, "src/env.mjs"), "utf8"), "text/javascript"],
  };
  const srv = createServer((req, res) => {
    const f = files[(req.url ?? "/").split("?")[0]];
    if (!f) return void res.writeHead(404).end();
    res.writeHead(200, { "content-type": f[1] }).end(f[0]);
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  srv.unref();
  pageBase = `http://localhost:${(srv.address() as AddressInfo).port}`;
});

const events = () => pg.psql(`select coalesce(string_agg(event, ',' order by event), '') from pa.events where project_id = '${site.project_id}'`);

test("Chromium: no request before consent; page view, event and click after it; typed text never stored", { skip: skipNoPg || (haveBrowser ? false : "no cached Chromium for playwright-core 1.63") }, async () => {
  // Automation flags make the SDK treat the page as a bot and drop its events; a real visitor has neither.
  // The headless shell reports a "HeadlessChrome" brand, so the full Chromium build (1243, cached) runs in new headless mode.
  const browser = await chromium.launch({ headless: true, channel: "chromium", args: ["--disable-blink-features=AutomationControlled"] });
  try {
    // The SDK drops events from headless user agents (bot filter), so present as the desktop browser it is.
    const page = await browser.newPage({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36" });
    const toHost: string[] = [];
    const consoleLines: string[] = [];
    page.on("console", (m) => consoleLines.push(`${m.type()}: ${m.text()}`));
    page.on("pageerror", (e) => consoleLines.push(`pageerror: ${e.message}`));
    page.on("requestfailed", (r) => consoleLines.push(`failed: ${r.url()} ${r.failure()?.errorText}`));
    page.on("request", (r) => {
      if (r.url().startsWith(run.base)) toHost.push(r.url().replace(run.base, ""));
    });
    await page.goto(pageBase);
    await page.waitForFunction(() => (window as unknown as { ready?: boolean }).ready === true);
    await page.fill("#email", "typed-in-chromium@shop.example");
    await page.click("#buy");
    await sleep(2000);
    assert.deepEqual(toHost, [], "no request to the analytics host before consent");
    assert.equal(await page.evaluate(() => localStorage.length), 0);
    assert.equal(await page.evaluate(() => document.cookie), "");

    await page.click("#allow");
    const until = Date.now() + 20_000;
    while (Date.now() < until && !/\$pageview/.test(events())) {
      await run.svc.deps.writer.flush();
      await sleep(250);
    }
    // Autocapture starts once the app's config has loaded.
    await page.waitForFunction(() => {
      const w = window as unknown as { analytics: { client: { autocapture?: { isEnabled?: boolean } } } };
      return w.analytics.client?.autocapture?.isEnabled === true;
    }, undefined, { timeout: 15_000 });
    await page.click("#buy");
    const until2 = Date.now() + 20_000;
    while (Date.now() < until2 && !(/\$autocapture/.test(events()) && /consented/.test(events()))) {
      await run.svc.deps.writer.flush();
      await sleep(250);
    }
    const got = events();
    if (!/\$pageview/.test(got)) {
      const state = await page.evaluate(() => {
        const c = (window as unknown as { analytics: { client: Record<string, any> } }).analytics.client;
        return c ? { host: c.config.api_host, optedOut: c.has_opted_out_capturing(), loaded: c.__loaded, dnt: navigator.doNotTrack, queue: c._requestQueue?._queue?.length } : null;
      });
      console.error(JSON.stringify({ state, consoleLines, toHost }, null, 1));
    }
    assert.match(got, /\$pageview/);
    assert.match(got, /consented/);
    assert.match(got, /\$autocapture/);
    assert.doesNotMatch(got, /before_consent/);
    assert.ok(toHost.some((u) => u.includes("/config")), "the app's config was read after consent");
    assert.equal(pg.psql(`select count(*) from pa.events where project_id = '${site.project_id}' and (properties::text like '%typed-in-chromium%' or coalesce(elements_chain, '') like '%typed-in-chromium%')`), "0");
    assert.match(pg.psql(`select elements_chain from pa.events where project_id = '${site.project_id}' and event = '$autocapture' limit 1`), /Buy now/);
    assert.equal(pg.psql(`select properties ->> '$current_url' from pa.events where project_id = '${site.project_id}' and event = '$pageview' limit 1`), `${pageBase}/`);
  } finally {
    await browser.close();
  }
});
