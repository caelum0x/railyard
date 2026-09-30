// One real Chromium run (the cached Chromium of playwright-core 1.63, nothing
// installed): a page with @railyard/analytics-js and the unmodified browser
// SDK records a session against the local service (the SDK loads its
// recorder from the service's /static/); the typed text is absent from the
// stored chunks; then the player page, opened through the signed link, plays
// the recording in the same browser.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import type { AddressInfo } from "node:net";
import { gunzipSync } from "fflate";
import { chromium } from "playwright-core";
import { MemoryObjectStore, useReplayStore } from "../../src/replay/object-store.ts";
import { makeSite, migratedPg, skipNoPg, type Running, type ScratchPg, startService } from "../a1/helpers.ts";
import { api, SECRET, waitFor } from "./helpers.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const PKG = join(HERE, "../../../../packages/analytics-js");
const SDK_ESM = join(PKG, "node_modules/posthog-js/dist/module.mjs");
const haveBrowser = existsSync(chromium.executablePath());
const TYPED = "typed-in-recording@shop.example";
const UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/145.0.0.0 Safari/537.36";

const store = new MemoryObjectStore();
let pg: ScratchPg;
let run: Running;
let pageBase = "";
const site = makeSite({ name: "shop" });

before(async () => {
  if (skipNoPg) return;
  useReplayStore(store);
  pg = await migratedPg();
  run = await startService(pg, [site], { consoleSecret: SECRET });
  const r = await api(run, site, "/recording-settings", { method: "PUT", body: { replay_enabled: true, replay_sample_rate: 1 } });
  assert.equal(r.status, 200);
  const page = `<!doctype html><html><head><title>Checkout</title>
<script type="importmap">{"imports":{"posthog-js":"/vendor/sdk.mjs"}}</script></head>
<body>
  <h1>Checkout</h1>
  <label>Email <input id="email" type="email"></label>
  <p data-private>private note 8841</p>
  <button id="buy">Buy now</button>
  <script type="module">
    import { createAnalytics } from "/pkg/index.mjs";
    window.analytics = createAnalytics({ site: ${JSON.stringify(site.token)}, host: ${JSON.stringify(run.base)}, requireConsent: false });
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
  await new Promise<void>((resolve) => srv.listen(0, "127.0.0.1", () => resolve()));
  srv.unref();
  pageBase = `http://localhost:${(srv.address() as AddressInfo).port}`;
});

test("Chromium: a recorded session is stored masked and plays back through the signed link", { skip: skipNoPg || (haveBrowser ? false : "no cached Chromium for playwright-core 1.63"), timeout: 120_000 }, async () => {
  const browser = await chromium.launch({ headless: true, channel: "chromium", args: ["--disable-blink-features=AutomationControlled"] });
  try {
    const page = await browser.newPage({ userAgent: UA });
    const toHost: string[] = [];
    page.on("request", (r) => {
      if (r.url().startsWith(run.base)) toHost.push(new URL(r.url()).pathname);
    });
    await page.goto(pageBase);
    await page.waitForFunction(() => (window as unknown as { ready?: boolean }).ready === true);
    await page.waitForFunction(() => {
      const c = (window as unknown as { analytics: { client: { sessionRecordingStarted?: () => boolean } | null } }).analytics.client;
      return c?.sessionRecordingStarted?.() === true;
    }, undefined, { timeout: 20_000 });
    await page.click("#email");
    await page.keyboard.type(TYPED, { delay: 5 });
    await page.click("#buy");
    await page.mouse.move(40, 60);
    await page.mouse.move(300, 200);
    const got = await waitFor(() => Number(pg.psql(`select coalesce(sum(event_count), 0) from pa.replays where project_id = '${site.project_id}'`)) >= 3, 30_000, 250);
    assert.ok(got, `recording stored (requests: ${[...new Set(toHost)].join(", ")})`);
    assert.ok(toHost.some((p) => /^\/static\/(.*\/)?lazy-recorder\.js$/.test(p)), `the recorder came from the service: ${[...new Set(toHost)].join(", ")}`);
    assert.ok(toHost.some((p) => p === "/s/" || p === "/s"), "snapshots went to /s/");
    const stored = [...store.objects.values()].map((o) => o.body);
    assert.ok(stored.length >= 1);
    for (const b of stored) assert.equal(Buffer.from(b).includes(Buffer.from(TYPED)), false);
    const text = stored.map((b) => new TextDecoder().decode(gunzipSync(b))).join("\n");
    assert.equal(text.includes(TYPED), false, "the typed address is not in the stored recording");
    assert.equal(text.includes("private note 8841"), false, "text under [data-private] is masked");
    assert.match(text, /Checkout/, "page text is recorded");
    assert.match(text, /"type":2,/, "a full page snapshot is recorded");

    const session = pg.psql(`select session_id from pa.replays where project_id = '${site.project_id}' order by started_at limit 1`);
    const link = await api(run, site, `/replays/${encodeURIComponent(session)}/link`, { body: {} });
    assert.equal(link.status, 200);
    const player = await browser.newPage();
    const errors: string[] = [];
    player.on("pageerror", (e) => errors.push(e.message));
    await player.goto(link.body.url);
    await player.waitForFunction(() => (window as unknown as { __replay?: { ready: boolean } }).__replay?.ready === true, undefined, { timeout: 20_000 });
    const state = await player.evaluate(() => {
      const s = (window as unknown as { __replay: { events: number; error: string | null } }).__replay;
      return { events: s.events, error: s.error, frames: document.querySelectorAll(".replayer-wrapper iframe").length, controller: !!document.querySelector(".rr-controller") };
    });
    assert.equal(state.error, null);
    assert.ok(state.events >= 3, `events played: ${state.events}`);
    assert.equal(state.frames, 1, "the player rebuilt the page in its frame");
    assert.equal(state.controller, true);
    const heading = await player.frameLocator(".replayer-wrapper iframe").locator("h1").textContent();
    assert.equal(heading, "Checkout", "the recorded page is shown");
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});
